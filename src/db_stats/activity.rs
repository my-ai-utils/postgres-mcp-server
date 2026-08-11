use std::time::Duration;

use my_postgres::tokio_postgres::Row;

use crate::postgres::{PostgresAccess, opt_f64, opt_i32, opt_i64, opt_string, stats_row};

/// `pg_stat_activity.backend_type` — the column that separates real client
/// connections from autovacuum workers and the WAL writer. Without it the
/// connection count is inflated by processes that do not consume a
/// `max_connections` slot.
const PG10: i32 = 100000;

/// Who is connected right now, and what is taking a long time.
#[derive(Debug, Clone)]
pub struct ActivityStats {
    /// Client backends across the **whole cluster** — this is what competes for
    /// `max_connections`, so counting only this database would understate the
    /// pressure.
    pub total_client_backends: Option<i64>,
    /// Client backends on this database alone.
    pub in_this_db: Option<i64>,
    pub active: Option<i64>,
    pub idle: Option<i64>,
    /// `idle in transaction` and `idle in transaction (aborted)`. These hold
    /// locks and pin the xmin horizon, so they are worth their own number rather
    /// than being folded into `idle`.
    pub idle_in_transaction: Option<i64>,
    /// Active backends currently blocked on a wait event.
    pub waiting: Option<i64>,
    /// Client backends whose `state` came back NULL.
    ///
    /// Postgres returns a row for every backend but blanks `state`, `query` and
    /// `wait_event` for backends belonging to other users unless the account is a
    /// member of `pg_monitor`/`pg_read_all_stats`. A non-zero value here means the
    /// four counts above are undercounts, which is why it is reported instead of
    /// being quietly folded into `idle`.
    pub state_unknown: Option<i64>,
    pub max_connections: Option<i64>,
    /// Longest-running active statements **on this database**, longest first.
    ///
    /// Scoped to this database deliberately, and not because of privileges:
    /// `pg_stat_activity` is cluster-wide, so an unscoped query would hand the
    /// agent on `/crm` the SQL text running on `/billing`. The counts above are
    /// cluster-wide because a number leaks nothing; query text does.
    pub longest: Vec<LongRunningQuery>,
}

#[derive(Debug, Clone)]
pub struct LongRunningQuery {
    pub pid: Option<i32>,
    pub user_name: Option<String>,
    pub application_name: Option<String>,
    pub state: Option<String>,
    /// `Lock: transactionid`, `IO: DataFileRead`, … `None` when the backend is
    /// running rather than waiting.
    pub wait: Option<String>,
    pub running_secs: Option<f64>,
    /// `None` when this account may not read another user's query text.
    pub query: Option<String>,
}

#[derive(Debug, Clone)]
struct ActivityCounts {
    total_client_backends: Option<i64>,
    in_this_db: Option<i64>,
    active: Option<i64>,
    idle: Option<i64>,
    idle_in_transaction: Option<i64>,
    waiting: Option<i64>,
    state_unknown: Option<i64>,
    max_connections: Option<i64>,
}

impl ActivityCounts {
    fn read_row(row: &Row) -> Self {
        Self {
            total_client_backends: opt_i64(row, "total_client_backends"),
            in_this_db: opt_i64(row, "in_this_db"),
            active: opt_i64(row, "active"),
            idle: opt_i64(row, "idle"),
            idle_in_transaction: opt_i64(row, "idle_in_transaction"),
            waiting: opt_i64(row, "waiting"),
            state_unknown: opt_i64(row, "state_unknown"),
            max_connections: opt_i64(row, "max_connections"),
        }
    }
}

stats_row!(ActivityCounts);

impl LongRunningQuery {
    fn read_row(row: &Row) -> Self {
        let wait_event_type = opt_string(row, "wait_event_type");
        let wait_event = opt_string(row, "wait_event");

        Self {
            pid: opt_i32(row, "pid"),
            user_name: opt_string(row, "user_name"),
            application_name: opt_string(row, "application_name"),
            state: opt_string(row, "state"),
            wait: match (wait_event_type, wait_event) {
                (Some(kind), Some(event)) => Some(format!("{}: {}", kind, event)),
                (Some(kind), None) => Some(kind),
                _ => None,
            },
            running_secs: opt_f64(row, "running_secs"),
            query: opt_string(row, "query"),
        }
    }
}

stats_row!(LongRunningQuery);

/// The `backend_type` filter is what makes these counts comparable to
/// `max_connections`; on a server too old to have the column everything is
/// counted instead, which overstates the total by the handful of background
/// processes rather than dropping the section.
fn counts_sql(server_version_num: i32) -> String {
    let client_backends = if server_version_num >= PG10 {
        "a.backend_type = 'client backend'"
    } else {
        "true"
    };

    format!(
        r#"
SELECT
    (count(*))::int8                                                                  AS total_client_backends,
    (count(*) FILTER (WHERE a.datname = current_database()))::int8                     AS in_this_db,
    (count(*) FILTER (WHERE a.state = 'active'))::int8                                 AS active,
    (count(*) FILTER (WHERE a.state = 'idle'))::int8                                   AS idle,
    (count(*) FILTER (WHERE a.state LIKE 'idle in transaction%'))::int8                AS idle_in_transaction,
    (count(*) FILTER (WHERE a.state = 'active' AND a.wait_event_type IS NOT NULL))::int8 AS waiting,
    (count(*) FILTER (WHERE a.state IS NULL))::int8                                    AS state_unknown,
    current_setting('max_connections')::int8                                           AS max_connections
FROM pg_stat_activity a
WHERE {}
"#,
        client_backends
    )
}

/// `EXTRACT(EPOCH FROM ...)` returns `numeric` from Postgres 16 on and
/// `double precision` before it, so the cast is not decoration — without it the
/// column type depends on the server version.
///
/// `pid <> pg_backend_pid()` drops this very query from its own results.
const LONGEST_SQL: &str = r#"
SELECT
    a.pid                                                        AS pid,
    a.usename::text                                              AS user_name,
    a.application_name                                           AS application_name,
    a.state                                                      AS state,
    a.wait_event_type                                            AS wait_event_type,
    a.wait_event                                                 AS wait_event,
    EXTRACT(EPOCH FROM (now() - a.query_start))::float8           AS running_secs,
    left(a.query, 2048)                                          AS query
FROM pg_stat_activity a
WHERE a.datname = current_database()
  AND a.state = 'active'
  AND a.query_start IS NOT NULL
  AND a.pid <> pg_backend_pid()
ORDER BY a.query_start ASC
LIMIT 5
"#;

pub async fn collect_activity(
    postgres: &PostgresAccess,
    server_version_num: i32,
    timeout: Duration,
) -> Result<ActivityStats, String> {
    let counts_sql = counts_sql(server_version_num);

    let counts: Vec<ActivityCounts> = postgres
        .query_typed("db_stats/activity", counts_sql.as_str(), timeout)
        .await?;

    let counts = counts
        .into_iter()
        .next()
        .ok_or_else(|| "The pg_stat_activity aggregate returned no row.".to_string())?;

    let longest: Vec<LongRunningQuery> = postgres
        .query_typed("db_stats/longest_queries", LONGEST_SQL, timeout)
        .await?;

    Ok(ActivityStats {
        total_client_backends: counts.total_client_backends,
        in_this_db: counts.in_this_db,
        active: counts.active,
        idle: counts.idle,
        idle_in_transaction: counts.idle_in_transaction,
        waiting: counts.waiting,
        state_unknown: counts.state_unknown,
        max_connections: counts.max_connections,
        longest,
    })
}
