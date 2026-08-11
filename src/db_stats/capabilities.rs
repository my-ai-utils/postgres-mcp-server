use std::time::Duration;

use my_postgres::tokio_postgres::Row;

use crate::postgres::{PostgresAccess, opt_bool, opt_i32, opt_string, stats_row};

/// What this connection is actually allowed to see, and what the server can
/// produce at all.
///
/// Collected first on every slow tick, because the other sections are gated on
/// it: `pg_stat_statements` columns were renamed in 13, `pg_stat_database` grew
/// its timing columns in 14, and the text of another backend's query is NULL
/// without `pg_monitor`. Guessing any of that and letting the query fail would
/// report "unavailable — column does not exist", which tells the operator nothing
/// about what to do; knowing it lets every refusal name the missing piece.
#[derive(Debug, Clone)]
pub struct ServerCapabilities {
    /// `16.3`, `14.11 (Debian ...)` — whatever `server_version` says.
    pub server_version: String,
    /// `160003`. The gate for every version-conditional query below.
    pub server_version_num: i32,
    pub database_name: String,
    pub user_name: String,
    pub is_superuser: bool,
    /// Member of `pg_monitor` or `pg_read_all_stats` (or superuser).
    ///
    /// Without it Postgres still returns a row for every backend and every
    /// statement, but blanks the columns that belong to other users — so the
    /// numbers are undercounts and the query texts are missing. That is the
    /// difference between an admin account and an ordinary one, and it is the
    /// only thing on this page that "is the account an admin?" actually decides.
    pub can_read_all_stats: bool,
    pub has_pg_stat_statements: bool,
    pub max_connections: i32,
    /// Whether the server measures how long I/O takes (`track_io_timing`).
    ///
    /// **Off by default**, because timing every block read costs something on some
    /// platforms. With it off `pg_stat_database.blk_read_time` and `blk_write_time`
    /// are permanently zero — not "no I/O happened", but "nobody was counting" —
    /// which is exactly the kind of zero this server refuses to publish as a number.
    pub track_io_timing: bool,
}

const SQL: &str = r#"
SELECT
    current_setting('server_version')                                        AS server_version,
    current_setting('server_version_num')::int4                              AS server_version_num,
    current_database()::text                                                 AS database_name,
    current_user::text                                                       AS user_name,
    current_setting('is_superuser') = 'on'                                   AS is_superuser,
    COALESCE(
        (
            SELECT bool_or(pg_has_role(current_user, r.oid, 'MEMBER'))
            FROM pg_roles r
            WHERE r.rolname IN ('pg_monitor', 'pg_read_all_stats')
        ),
        false
    )                                                                        AS can_read_all_stats,
    EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'pg_stat_statements')  AS has_pg_stat_statements,
    current_setting('max_connections')::int4                                  AS max_connections,
    current_setting('track_io_timing') = 'on'                                 AS track_io_timing
"#;

impl ServerCapabilities {
    /// `unwrap_or_default()` here is not the silent fallback it looks like: none
    /// of the expressions above can return NULL. `current_setting` on a built-in
    /// GUC always has a value, `= 'on'` on a non-NULL left side is never NULL,
    /// `COALESCE(..., false)` and `EXISTS` are NULL-free by construction. A
    /// `None` at this point would mean the driver handed back a column of the
    /// wrong type, which is a bug in this query, not a state of the database —
    /// and the fields it feeds are only ever used to *withhold* a section, so the
    /// pessimistic default is also the safe one.
    fn read_row(row: &Row) -> Self {
        Self {
            server_version: opt_string(row, "server_version").unwrap_or_default(),
            server_version_num: opt_i32(row, "server_version_num").unwrap_or_default(),
            database_name: opt_string(row, "database_name").unwrap_or_default(),
            user_name: opt_string(row, "user_name").unwrap_or_default(),
            is_superuser: opt_bool(row, "is_superuser").unwrap_or_default(),
            can_read_all_stats: opt_bool(row, "can_read_all_stats").unwrap_or_default(),
            has_pg_stat_statements: opt_bool(row, "has_pg_stat_statements").unwrap_or_default(),
            max_connections: opt_i32(row, "max_connections").unwrap_or_default(),
            track_io_timing: opt_bool(row, "track_io_timing").unwrap_or_default(),
        }
    }

    /// `pg_monitor`/`pg_read_all_stats` membership is inherited by superusers
    /// without an explicit grant, so the two have to be checked together.
    pub fn sees_all_stats(&self) -> bool {
        self.is_superuser || self.can_read_all_stats
    }
}

stats_row!(ServerCapabilities);

pub async fn collect_capabilities(
    postgres: &PostgresAccess,
    timeout: Duration,
) -> Result<ServerCapabilities, String> {
    let rows: Vec<ServerCapabilities> = postgres
        .query_typed("db_stats/capabilities", SQL, timeout)
        .await?;

    rows.into_iter()
        .next()
        .ok_or_else(|| "The server capabilities query returned no row.".to_string())
}
