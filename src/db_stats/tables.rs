use std::time::Duration;

use my_postgres::tokio_postgres::Row;

use crate::postgres::{PostgresAccess, opt_i64, opt_string, opt_timestamp, stats_row};

/// How many tables the card carries. The point of the card is "what is big and
/// what needs vacuuming", and both questions are answered by the head of the
/// list; a thousand-table schema would otherwise put a megabyte of names through
/// the admin API every minute and into the agent's context on every tool call.
const TOP_N: usize = 25;

#[derive(Debug, Clone)]
pub struct TablesStats {
    /// Every table in the database, not just the [`TOP_N`] returned — so the UI
    /// can say "top 25 of 812" instead of implying the list is complete.
    pub table_count: Option<i64>,
    pub items: Vec<TableStats>,
}

#[derive(Debug, Clone)]
pub struct TableStats {
    pub schema_name: Option<String>,
    pub table_name: Option<String>,
    /// Heap + indexes + TOAST. What the table actually costs on disk.
    pub total_bytes: Option<i64>,
    /// Heap + TOAST, without indexes.
    pub table_bytes: Option<i64>,
    pub index_bytes: Option<i64>,
    /// Planner estimates from `pg_stat_user_tables`, not `count(*)` — exact counts
    /// would mean a full scan of every table on the list, once a minute.
    pub live_tuples: Option<i64>,
    pub dead_tuples: Option<i64>,
    pub seq_scans: Option<i64>,
    /// `None` on a table no index has ever been used on — which is exactly the
    /// case worth noticing, and why a NULL is not turned into a 0 here.
    pub idx_scans: Option<i64>,
    /// The later of the manual and the auto run.
    pub last_vacuum: Option<String>,
    pub last_analyze: Option<String>,
}

impl TableStats {
    fn read_row(row: &Row) -> Self {
        Self {
            schema_name: opt_string(row, "schema_name"),
            table_name: opt_string(row, "table_name"),
            total_bytes: opt_i64(row, "total_bytes"),
            table_bytes: opt_i64(row, "table_bytes"),
            index_bytes: opt_i64(row, "index_bytes"),
            live_tuples: opt_i64(row, "live_tuples"),
            dead_tuples: opt_i64(row, "dead_tuples"),
            seq_scans: opt_i64(row, "seq_scans"),
            idx_scans: opt_i64(row, "idx_scans"),
            last_vacuum: opt_timestamp(row, "last_vacuum"),
            last_analyze: opt_timestamp(row, "last_analyze"),
        }
    }
}

/// One row of [`build_sql`]: the table, plus the database-wide table count that
/// the window function repeats on every row.
struct TableStatsRow {
    table: TableStats,
    table_count: Option<i64>,
}

impl TableStatsRow {
    fn read_row(row: &Row) -> Self {
        Self {
            table: TableStats::read_row(row),
            table_count: opt_i64(row, "table_count"),
        }
    }
}

stats_row!(TableStatsRow);

/// `count(*) OVER ()` is evaluated before `LIMIT`, so one query returns both the
/// top slice and the true total.
///
/// `relkind` covers ordinary tables (`r`) and materialized views (`m`), both of
/// which occupy real storage. Partitioned parents (`p`) are left out on purpose:
/// they hold no data of their own, and their partitions are `r` and appear in
/// their own right — including the parent would add a row of zeroes per
/// partitioned table. `pg_stat_user_tables` has no row for a materialized view,
/// which is what the LEFT JOIN is for.
///
/// The `\_` in the TOAST filter escapes LIKE's single-character wildcard: an
/// unescaped `pg_toast%` would also match a user schema called `pgxtoast`.
fn build_sql() -> String {
    format!(
        r#"
SELECT
    (count(*) OVER ())::int8                                     AS table_count,
    n.nspname::text                                              AS schema_name,
    c.relname::text                                              AS table_name,
    pg_total_relation_size(c.oid)                                AS total_bytes,
    pg_table_size(c.oid)                                         AS table_bytes,
    pg_indexes_size(c.oid)                                       AS index_bytes,
    s.n_live_tup                                                 AS live_tuples,
    s.n_dead_tup                                                 AS dead_tuples,
    s.seq_scan                                                   AS seq_scans,
    s.idx_scan                                                   AS idx_scans,
    GREATEST(s.last_vacuum, s.last_autovacuum)                   AS last_vacuum,
    GREATEST(s.last_analyze, s.last_autoanalyze)                 AS last_analyze
FROM pg_class c
JOIN pg_namespace n ON n.oid = c.relnamespace
LEFT JOIN pg_stat_user_tables s ON s.relid = c.oid
WHERE c.relkind IN ('r', 'm')
  AND n.nspname NOT IN ('pg_catalog', 'information_schema')
  AND n.nspname NOT LIKE 'pg\_toast%'
ORDER BY pg_total_relation_size(c.oid) DESC
LIMIT {}
"#,
        TOP_N
    )
}

pub async fn collect_tables(postgres: &PostgresAccess, timeout: Duration) -> Result<TablesStats, String> {
    let sql = build_sql();

    let rows: Vec<TableStatsRow> = postgres
        .query_typed("db_stats/tables", sql.as_str(), timeout)
        .await?;

    // The window function repeats the total on every row, so any row will do. No
    // rows means the database genuinely has no tables — a real zero, not a
    // missing value.
    let table_count = rows.first().and_then(|row| row.table_count).or(Some(0));

    Ok(TablesStats {
        table_count,
        items: rows.into_iter().map(|row| row.table).collect(),
    })
}
