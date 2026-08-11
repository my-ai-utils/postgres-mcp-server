use std::collections::HashMap;
use std::time::Duration;

use my_postgres::tokio_postgres::Row;
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::postgres::{PostgresAccess, opt_f64, opt_i64, opt_string, stats_row};

use super::ServerCapabilities;

/// `pg_stat_statements` renamed `total_time`/`mean_time` to
/// `total_exec_time`/`mean_exec_time` in 13, when planning time was split out.
/// Rather than probe for both spellings, the section is withheld below 13 with a
/// reason that says so — 12 has been out of support since November 2024.
const PG13: i32 = 130000;

/// Rows pulled from the view. More than the [`TOP_N`] published, because the list
/// is re-sorted by *recent* execution time afterwards: a statement that only
/// started burning time in the last minute can sit well below 25th place by
/// lifetime total, and taking exactly 25 by lifetime would never let it surface.
const FETCH_N: usize = 60;

const TOP_N: usize = 25;

pub const NO_EXTENSION: &str = "pg_stat_statements is not installed on this server. \
    Install it (shared_preload_libraries = 'pg_stat_statements', then CREATE EXTENSION \
    pg_stat_statements) to see which statements consume the most execution time.";

pub fn too_old(version: &str) -> String {
    format!(
        "pg_stat_statements on Postgres {} does not have the total_exec_time column (renamed in \
         13), so per-statement execution time cannot be read.",
        version
    )
}

/// One statement's counters as the view reports them — cumulative since the
/// extension was reset.
#[derive(Debug, Clone)]
struct StatementSample {
    query_id: Option<i64>,
    calls: Option<i64>,
    total_exec_ms: Option<f64>,
    mean_exec_ms: Option<f64>,
    rows_returned: Option<i64>,
    blks_hit: Option<i64>,
    blks_read: Option<i64>,
    query: Option<String>,
}

impl StatementSample {
    fn read_row(row: &Row) -> Self {
        Self {
            query_id: opt_i64(row, "query_id"),
            calls: opt_i64(row, "calls"),
            total_exec_ms: opt_f64(row, "total_exec_ms"),
            mean_exec_ms: opt_f64(row, "mean_exec_ms"),
            rows_returned: opt_i64(row, "rows_returned"),
            blks_hit: opt_i64(row, "blks_hit"),
            blks_read: opt_i64(row, "blks_read"),
            query: opt_string(row, "query"),
        }
    }
}

stats_row!(StatementSample);

/// The previous tick's counters, keyed by `queryid`, so the next tick can report
/// what moved rather than what has accumulated since the extension was installed.
#[derive(Debug, Clone)]
pub struct StatementsSnapshot {
    taken_at: DateTimeAsMicroseconds,
    by_query_id: HashMap<i64, (Option<i64>, Option<f64>)>,
}

#[derive(Debug, Clone)]
pub struct TopStatements {
    /// Whether the account can see other users' statements. When false the list
    /// only covers what this connection's own role executed, which for this
    /// server means "the SQL the agent has been running" and nothing else.
    pub sees_all_statements: bool,
    pub items: Vec<TopStatement>,
}

#[derive(Debug, Clone)]
pub struct TopStatement {
    pub query_id: Option<i64>,
    pub calls: Option<i64>,
    pub total_exec_ms: Option<f64>,
    pub mean_exec_ms: Option<f64>,
    pub rows_returned: Option<i64>,
    pub blks_hit: Option<i64>,
    pub blks_read: Option<i64>,
    /// Normalized statement text with the constants replaced by `$1`, `$2`, …
    /// `None`, or the literal `<insufficient privilege>`, when the statement
    /// belongs to another user and this account is not a member of
    /// `pg_monitor`/`pg_read_all_stats`.
    pub query: Option<String>,
    pub delta_calls: Option<i64>,
    pub delta_exec_ms: Option<f64>,
    /// Milliseconds of execution per wall-clock second in the window since the
    /// previous tick — the per-statement counterpart of
    /// [`super::DbHealthRates::busy_backends`]. 1000 means this one statement kept
    /// a backend busy continuously.
    pub exec_ms_per_sec: Option<f64>,
}

/// `dbid` scopes the view to this mount's database. `pg_stat_statements` is
/// cluster-wide, and an unscoped list would put the SQL running against the other
/// configured databases into this endpoint's response — the one thing the mount
/// separation exists to prevent.
fn build_sql() -> String {
    format!(
        r#"
SELECT
    s.queryid                                                    AS query_id,
    s.calls                                                      AS calls,
    s.total_exec_time                                            AS total_exec_ms,
    s.mean_exec_time                                             AS mean_exec_ms,
    s.rows                                                       AS rows_returned,
    s.shared_blks_hit                                            AS blks_hit,
    s.shared_blks_read                                           AS blks_read,
    left(s.query, 1024)                                          AS query
FROM pg_stat_statements s
WHERE s.dbid = (SELECT oid FROM pg_database WHERE datname = current_database())
ORDER BY s.total_exec_time DESC
LIMIT {}
"#,
        FETCH_N
    )
}

/// Same reset/rollback rule as [`super::health`]: a counter that went backwards
/// means the extension was reset, and the honest answer for that window is "not
/// known" rather than a negative rate or a fabricated zero.
fn delta_i64(current: Option<i64>, previous: Option<i64>) -> Option<i64> {
    let (current, previous) = (current?, previous?);

    if current < previous {
        return None;
    }

    Some(current - previous)
}

fn delta_f64(current: Option<f64>, previous: Option<f64>) -> Option<f64> {
    let (current, previous) = (current?, previous?);

    if current < previous {
        return None;
    }

    Some(current - previous)
}

/// Turns the raw samples into the published list and the snapshot the *next* tick
/// will diff against.
fn build(
    samples: Vec<StatementSample>,
    previous: Option<&StatementsSnapshot>,
    sees_all_statements: bool,
) -> (TopStatements, StatementsSnapshot) {
    let taken_at = DateTimeAsMicroseconds::now();

    let window_secs = previous
        .map(|previous| taken_at.duration_since(previous.taken_at).get_full_seconds() as f64)
        .filter(|secs| *secs >= 1.0);

    let mut by_query_id = HashMap::with_capacity(samples.len());

    let mut items: Vec<TopStatement> = samples
        .into_iter()
        .map(|sample| {
            // A statement with no readable queryid cannot be matched across ticks,
            // so it gets lifetime figures only rather than being paired with an
            // arbitrary neighbour.
            let earlier = sample
                .query_id
                .zip(previous)
                .and_then(|(query_id, previous)| previous.by_query_id.get(&query_id).copied());

            if let Some(query_id) = sample.query_id {
                by_query_id.insert(query_id, (sample.calls, sample.total_exec_ms));
            }

            let (delta_calls, delta_exec_ms) = match earlier {
                Some((calls, total_exec_ms)) => (
                    delta_i64(sample.calls, calls),
                    delta_f64(sample.total_exec_ms, total_exec_ms),
                ),
                None => (None, None),
            };

            TopStatement {
                query_id: sample.query_id,
                calls: sample.calls,
                total_exec_ms: sample.total_exec_ms,
                mean_exec_ms: sample.mean_exec_ms,
                rows_returned: sample.rows_returned,
                blks_hit: sample.blks_hit,
                blks_read: sample.blks_read,
                query: sample.query,
                delta_calls,
                delta_exec_ms,
                exec_ms_per_sec: delta_exec_ms
                    .zip(window_secs)
                    .map(|(delta_ms, secs)| delta_ms / secs),
            }
        })
        .collect();

    // Sort by what moved in the last window, with the lifetime total as the tiebreak
    // — which is the whole ordering on the first tick, where nothing has a delta yet.
    // Unconditional rather than only when some delta exists: leaning on the query's
    // ORDER BY for that case would make the ranking depend on which SQL ran.
    items.sort_by(|left, right| {
        let key = |item: &TopStatement| {
            (
                item.delta_exec_ms.unwrap_or(0.0),
                item.total_exec_ms.unwrap_or(0.0),
            )
        };

        key(right)
            .partial_cmp(&key(left))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    items.truncate(TOP_N);

    (
        TopStatements {
            sees_all_statements,
            items,
        },
        StatementsSnapshot {
            taken_at,
            by_query_id,
        },
    )
}

/// `Ok(None)` means the section is unavailable for a reason worth reporting; the
/// caller turns the string into [`super::Section::Unavailable`].
pub async fn collect_statements(
    postgres: &PostgresAccess,
    capabilities: &ServerCapabilities,
    previous: Option<&StatementsSnapshot>,
    timeout: Duration,
) -> Result<(TopStatements, StatementsSnapshot), String> {
    if !capabilities.has_pg_stat_statements {
        return Err(NO_EXTENSION.to_string());
    }

    if capabilities.server_version_num < PG13 {
        return Err(too_old(capabilities.server_version.as_str()));
    }

    let sql = build_sql();

    let samples: Vec<StatementSample> = postgres
        .query_typed("db_stats/statements", sql.as_str(), timeout)
        .await
        // The extension can be installed into a schema that is not on this
        // connection's search_path, in which case the view is simply not visible
        // under its bare name. That reads as a confusing "relation does not exist"
        // unless it is spelled out.
        .map_err(|err| {
            format!(
                "{} (pg_stat_statements is installed but this query failed — check that its \
                 schema is on the connection's search_path)",
                err
            )
        })?;

    Ok(build(samples, previous, capabilities.sees_all_stats()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(query_id: i64, calls: i64, total_exec_ms: f64) -> StatementSample {
        StatementSample {
            query_id: Some(query_id),
            calls: Some(calls),
            total_exec_ms: Some(total_exec_ms),
            mean_exec_ms: Some(total_exec_ms / calls as f64),
            rows_returned: Some(calls),
            blks_hit: Some(0),
            blks_read: Some(0),
            query: Some(format!("SELECT {}", query_id)),
        }
    }

    fn snapshot_at(secs: i64, entries: &[(i64, i64, f64)]) -> StatementsSnapshot {
        StatementsSnapshot {
            taken_at: DateTimeAsMicroseconds::new(secs * 1_000_000),
            by_query_id: entries
                .iter()
                .map(|(id, calls, total)| (*id, (Some(*calls), Some(*total))))
                .collect(),
        }
    }

    #[test]
    fn the_first_tick_reports_lifetime_totals_and_no_deltas() {
        let (top, snapshot) = build(vec![sample(1, 10, 5_000.0)], None, true);

        assert_eq!(top.items.len(), 1);
        assert_eq!(top.items[0].total_exec_ms, Some(5_000.0));
        assert_eq!(top.items[0].delta_exec_ms, None);
        assert_eq!(top.items[0].exec_ms_per_sec, None);
        // ...but the snapshot is armed for the next tick.
        assert_eq!(snapshot.by_query_id.len(), 1);
    }

    #[test]
    fn a_statement_that_only_woke_up_recently_outranks_a_bigger_lifetime_total() {
        // #1 has burned far more time overall but nothing since the last tick;
        // #2 burned 30s in the last 60s.
        let previous = snapshot_at(0, &[(1, 1_000, 900_000.0), (2, 5, 100.0)]);

        let (top, _) = build(
            vec![sample(1, 1_000, 900_000.0), sample(2, 400, 30_100.0)],
            Some(&previous),
            true,
        );

        assert_eq!(top.items[0].query_id, Some(2));
        assert_eq!(top.items[0].delta_exec_ms, Some(30_000.0));
        assert_eq!(top.items[1].query_id, Some(1));
        assert_eq!(top.items[1].delta_exec_ms, Some(0.0));
    }

    #[test]
    fn exec_ms_per_sec_is_the_delta_over_the_window() {
        // taken_at is `now()`, so the window is anchored a known distance back.
        let now = DateTimeAsMicroseconds::now();
        let previous = StatementsSnapshot {
            taken_at: DateTimeAsMicroseconds::new(now.unix_microseconds - 10_000_000),
            by_query_id: [(1i64, (Some(0i64), Some(0.0f64)))].into_iter().collect(),
        };

        let (top, _) = build(vec![sample(1, 100, 5_000.0)], Some(&previous), true);

        // 5s of execution in a ~10s window.
        let per_sec = top.items[0].exec_ms_per_sec.unwrap();
        assert!(
            (per_sec - 500.0).abs() < 60.0,
            "expected ~500 ms/s, got {}",
            per_sec
        );
    }

    #[test]
    fn a_reset_extension_drops_the_delta_rather_than_going_negative() {
        let previous = snapshot_at(0, &[(1, 1_000, 900_000.0)]);

        let (top, _) = build(vec![sample(1, 3, 12.0)], Some(&previous), true);

        assert_eq!(top.items[0].delta_calls, None);
        assert_eq!(top.items[0].delta_exec_ms, None);
        assert_eq!(top.items[0].exec_ms_per_sec, None);
        // The lifetime figures are still the truth as the view reports it.
        assert_eq!(top.items[0].total_exec_ms, Some(12.0));
    }

    #[test]
    fn a_statement_with_no_readable_query_id_gets_lifetime_figures_only() {
        let previous = snapshot_at(0, &[(1, 10, 100.0)]);
        let mut hidden = sample(1, 500, 50_000.0);
        hidden.query_id = None;

        let (top, snapshot) = build(vec![hidden], Some(&previous), false);

        assert_eq!(top.items[0].delta_exec_ms, None);
        assert!(snapshot.by_query_id.is_empty());
        assert!(!top.sees_all_statements);
    }

    #[test]
    fn the_published_list_is_capped_at_top_n() {
        let samples = (0..FETCH_N as i64)
            .map(|i| sample(i, 10, (FETCH_N as f64) - i as f64))
            .collect();

        let (top, _) = build(samples, None, true);

        assert_eq!(top.items.len(), TOP_N);
    }
}
