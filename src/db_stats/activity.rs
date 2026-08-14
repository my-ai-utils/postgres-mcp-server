use std::time::Duration;

use my_postgres::tokio_postgres::Row;

use crate::postgres::{
    PostgresAccess, opt_bool, opt_f64, opt_i32, opt_i64, opt_string, opt_timestamp, stats_row,
};

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
    /// pressure. Includes this server's own collector connection, which does hold a
    /// slot.
    pub total_client_backends: Option<i64>,
    /// Client backends on this database alone.
    pub in_this_db: Option<i64>,
    /// The four counts below are **this database only**, and all exclude the
    /// collector's own backend — see [`counts_sql`]. They therefore do not add up
    /// to `in_this_db`, and are not meant to.
    pub active: Option<i64>,
    pub idle: Option<i64>,
    /// `idle in transaction` and `idle in transaction (aborted)`. These hold
    /// locks and pin the xmin horizon, so they are worth their own number rather
    /// than being folded into `idle`.
    pub idle_in_transaction: Option<i64>,
    /// Active backends currently blocked on a wait event.
    pub waiting: Option<i64>,
    /// Backends on this database whose `state` came back NULL.
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
    /// Every client backend on **this database**, busiest first — see
    /// [`connections_sql`]. Scoped to this database for the same reason
    /// [`Self::longest`] is: the row carries query text.
    ///
    /// Truncated at [`CONNECTIONS_LIMIT`] rows. [`Self::in_this_db`] is the total it
    /// was cut from, so a reader can always tell a complete list from a capped one.
    pub connections: Vec<BackendConnection>,
}

#[derive(Debug, Clone)]
pub struct LongRunningQuery {
    pub pid: Option<i32>,
    /// When this statement started.
    ///
    /// Carried so that the same execution seen across several 5-second ticks can be
    /// recognised as one. `pid` alone is not enough — Postgres hands the same
    /// backend to the next statement, so two unrelated executions would collapse
    /// into one; `(pid, query_start)` identifies an execution.
    pub query_start: Option<String>,
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

impl LongRunningQuery {
    /// Identity of one *execution*, for deduplicating across ticks. `None` when
    /// either half is missing, in which case the caller cannot safely merge it with
    /// anything and should treat it as its own sighting.
    pub fn execution_key(&self) -> Option<(i32, String)> {
        Some((self.pid?, self.query_start.clone()?))
    }
}

/// One client backend, whatever it is doing — including the ones doing nothing.
///
/// [`LongRunningQuery`] answers "what is slow"; this answers the other question an
/// operator asks of `pg_stat_activity`: *who is holding a connection*. The two
/// overlap on the active backends and are collected separately on purpose — the
/// longest list feeds the hourly history and keeps its own, wider slice of query
/// text, while this one is a snapshot that is thrown away on the next tick.
#[derive(Debug, Clone)]
pub struct BackendConnection {
    pub pid: Option<i32>,
    pub user_name: Option<String>,
    pub application_name: Option<String>,
    /// `None` for a backend connected over the unix socket — which is what a
    /// Postgres running next to its client looks like, not a missing value.
    pub client_addr: Option<String>,
    /// `active`, `idle`, `idle in transaction`, … `None` when this account may not
    /// read another user's state; see [`ActivityStats::state_unknown`].
    pub state: Option<String>,
    /// `Lock: transactionid`, `IO: DataFileRead`, … `None` when not waiting.
    pub wait: Option<String>,
    pub backend_start: Option<String>,
    /// Age of the connection itself, not of what it is running.
    pub connected_secs: Option<f64>,
    /// How long the backend has been in its current state. The number that matters
    /// for `idle in transaction`, where it is the age of a transaction still holding
    /// locks and pinning the xmin horizon.
    pub state_secs: Option<f64>,
    /// How long the current statement has been running. For an idle backend this is
    /// the age of the statement it ran *last*, which is why the state is always read
    /// alongside it.
    pub running_secs: Option<f64>,
    /// The current statement, or — on an idle backend — the last one it ran. `None`
    /// when this account may not read another user's query text.
    pub query: Option<String>,
    /// This server's own collector connection. Marked rather than hidden: it does
    /// hold a `max_connections` slot, so dropping it would make the list disagree
    /// with [`ActivityStats::in_this_db`] by exactly one and leave nobody able to
    /// explain the difference.
    pub is_collector: bool,
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

/// `Lock: transactionid` out of the two columns Postgres splits it across.
///
/// `wait_event` without its type is meaningless — `ClientRead` and `DataFileRead`
/// are both just "Read" — so the two are joined here rather than travelling as a
/// pair every consumer would have to reassemble.
fn read_wait(row: &Row) -> Option<String> {
    let wait_event_type = opt_string(row, "wait_event_type");
    let wait_event = opt_string(row, "wait_event");

    match (wait_event_type, wait_event) {
        (Some(kind), Some(event)) => Some(format!("{}: {}", kind, event)),
        (Some(kind), None) => Some(kind),
        _ => None,
    }
}

impl LongRunningQuery {
    fn read_row(row: &Row) -> Self {
        Self {
            pid: opt_i32(row, "pid"),
            query_start: opt_timestamp(row, "query_start"),
            user_name: opt_string(row, "user_name"),
            application_name: opt_string(row, "application_name"),
            state: opt_string(row, "state"),
            wait: read_wait(row),
            running_secs: opt_f64(row, "running_secs"),
            query: opt_string(row, "query"),
        }
    }
}

stats_row!(LongRunningQuery);

impl BackendConnection {
    fn read_row(row: &Row) -> Self {
        Self {
            pid: opt_i32(row, "pid"),
            user_name: opt_string(row, "user_name"),
            application_name: opt_string(row, "application_name"),
            client_addr: opt_string(row, "client_addr"),
            state: opt_string(row, "state"),
            wait: read_wait(row),
            backend_start: opt_timestamp(row, "backend_start"),
            connected_secs: opt_f64(row, "connected_secs"),
            state_secs: opt_f64(row, "state_secs"),
            running_secs: opt_f64(row, "running_secs"),
            query: opt_string(row, "query"),
            // A column the server always computes, so a missing value can only mean
            // the row did not decode — and calling a backend "ours" on that basis
            // would be worse than calling it someone else's.
            is_collector: opt_bool(row, "is_collector").unwrap_or(false),
        }
    }
}

stats_row!(BackendConnection);

/// The `backend_type` filter is what makes the totals comparable to
/// `max_connections`; on a server too old to have the column everything is
/// counted instead, which overstates the total by the handful of background
/// processes rather than dropping the section.
///
/// # Two scopes in one row, on purpose
///
/// `pg_stat_activity` is cluster-wide, and the two scopes answer different
/// questions, so both are collected and each is named for what it is:
///
/// - `total_client_backends` is **cluster-wide**, because that is what competes for
///   `max_connections`. Counting only this database would understate the pressure
///   that actually causes "too many clients already".
/// - the state breakdown is **this database only**. It is published on a
///   per-database card next to that database's tables and longest queries, and a
///   cluster-wide `active` there would contradict the very list under it.
///
/// # Why the state counts exclude our own backend
///
/// This query runs from a connection which is, at that instant, `active` — running
/// this query. Counted, it would report at least one active backend on every tick,
/// forever, on a completely idle database: the metric observing itself. It is still
/// counted in the totals, because it does hold a real connection slot.
fn counts_sql(server_version_num: i32) -> String {
    let client_backends = if server_version_num >= PG10 {
        "a.backend_type = 'client backend'"
    } else {
        "true"
    };

    // Every state count is scoped to this database AND excludes the collector.
    let here = "a.datname = current_database() AND a.pid <> pg_backend_pid()";

    format!(
        r#"
SELECT
    (count(*))::int8                                                                      AS total_client_backends,
    (count(*) FILTER (WHERE a.datname = current_database()))::int8                         AS in_this_db,
    (count(*) FILTER (WHERE {here} AND a.state = 'active'))::int8                          AS active,
    (count(*) FILTER (WHERE {here} AND a.state = 'idle'))::int8                            AS idle,
    (count(*) FILTER (WHERE {here} AND a.state LIKE 'idle in transaction%'))::int8         AS idle_in_transaction,
    (count(*) FILTER (WHERE {here} AND a.state = 'active'
                        AND a.wait_event_type IS NOT NULL))::int8                          AS waiting,
    (count(*) FILTER (WHERE {here} AND a.state IS NULL))::int8                             AS state_unknown,
    current_setting('max_connections')::int8                                               AS max_connections
FROM pg_stat_activity a
WHERE {client_backends}
"#,
        here = here,
        client_backends = client_backends
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
    a.query_start                                                AS query_start,
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

/// Rows in the connection list. A pool that has run away can leave hundreds of
/// backends on one database, and this list is rendered as a table and handed to an
/// MCP tool — both of which a thousand rows would drown rather than inform.
///
/// The cap is not a silent truncation: the ordering below puts the backends worth
/// looking at first, and `in_this_db` travels next to the list as the total it was
/// cut from.
pub const CONNECTIONS_LIMIT: usize = 100;

/// How much of each backend's statement the list carries.
///
/// Shorter than the 2048 characters [`LONGEST_SQL`] keeps, because this list is up
/// to [`CONNECTIONS_LIMIT`] rows collected every 5 seconds rather than 5, and every
/// one of them carries text. Enough to recognise a statement, which is what this
/// column is for; the full text of the ones that matter is on the longest list.
const CONNECTION_QUERY_CHARS: usize = 512;

/// Who is holding a connection, ordered so that the first rows are the ones worth
/// looking at.
///
/// # The ordering
///
/// Active first, then `idle in transaction`, then everything else — and inside each
/// group, longest in that state first. That is the order in which the rows answer a
/// question: an active backend's age is how long its statement has run, an idle-in-
/// transaction backend's age is how long it has been holding locks and pinning the
/// xmin horizon, and an idle backend's age is only ever "this pool is oversized".
/// Backends whose state is invisible to this account sort last, since nothing can be
/// said about them at all.
///
/// It is also what makes [`CONNECTIONS_LIMIT`] safe to apply: the rows dropped by
/// the cap are the least interesting ones, not an arbitrary hundred.
///
/// # Scope
///
/// This database only — the rows carry query text, and `pg_stat_activity` is
/// cluster-wide; see [`ActivityStats::longest`].
///
/// Unlike the state counts, this list keeps the collector's own backend, flagged as
/// `is_collector`. It holds a real connection slot, so hiding it would make the list
/// one shorter than `in_this_db` with nothing on the page to explain why.
///
/// # `clock_timestamp()`, not `now()`
///
/// `now()` is the *transaction's* start time, which is fractionally earlier than the
/// moment the statement began — so the row for this very query, the one the line
/// above deliberately keeps, comes back with a **negative** age. Measured at
/// -0.0005s against a live server, which is small and completely wrong: it is the
/// one backend in the list whose statement provably started after the snapshot.
/// `clock_timestamp()` reads the wall clock as each row is built, which is what
/// "how long has this been running" means.
fn connections_sql(server_version_num: i32) -> String {
    let client_backends = if server_version_num >= PG10 {
        "AND a.backend_type = 'client backend'"
    } else {
        ""
    };

    format!(
        r#"
SELECT
    a.pid                                                        AS pid,
    a.usename::text                                              AS user_name,
    a.application_name                                           AS application_name,
    host(a.client_addr)                                          AS client_addr,
    a.state                                                      AS state,
    a.wait_event_type                                            AS wait_event_type,
    a.wait_event                                                 AS wait_event,
    a.backend_start                                              AS backend_start,
    EXTRACT(EPOCH FROM (clock_timestamp() - a.backend_start))::float8  AS connected_secs,
    EXTRACT(EPOCH FROM (clock_timestamp() - a.state_change))::float8   AS state_secs,
    EXTRACT(EPOCH FROM (clock_timestamp() - a.query_start))::float8    AS running_secs,
    left(a.query, {query_chars})                                  AS query,
    (a.pid = pg_backend_pid())                                    AS is_collector
FROM pg_stat_activity a
WHERE a.datname = current_database()
  {client_backends}
ORDER BY
    CASE
        WHEN a.state = 'active' THEN 0
        WHEN a.state LIKE 'idle in transaction%' THEN 1
        WHEN a.state IS NULL THEN 3
        ELSE 2
    END,
    a.state_change ASC NULLS LAST
LIMIT {limit}
"#,
        query_chars = CONNECTION_QUERY_CHARS,
        client_backends = client_backends,
        limit = CONNECTIONS_LIMIT
    )
}

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

    let connections_sql = connections_sql(server_version_num);

    let connections: Vec<BackendConnection> = postgres
        .query_typed("db_stats/connections", connections_sql.as_str(), timeout)
        .await?;

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
        connections,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Postgres 9.6, the last release without `backend_type`.
    const PG96: i32 = 90600;

    #[test]
    fn the_connection_list_never_leaves_this_database() {
        // The rows carry query text and pg_stat_activity is cluster-wide, so an
        // unscoped list would hand the agent on /crm what is running on /billing.
        for version in [PG96, PG10] {
            assert!(
                connections_sql(version).contains("a.datname = current_database()"),
                "the connection list must be scoped to this database"
            );
        }
    }

    #[test]
    fn only_a_server_with_backend_type_filters_on_it() {
        // Filtering on a column that does not exist yet would turn the whole section
        // into an error on 9.6; counting the background workers too only overstates
        // it by a handful of rows.
        assert!(connections_sql(PG10).contains("backend_type = 'client backend'"));
        assert!(!connections_sql(PG96).contains("backend_type"));
    }

    #[test]
    fn the_list_is_capped_and_ordered_so_the_cap_drops_the_dullest_rows() {
        let sql = connections_sql(PG10);

        assert!(sql.contains(&format!("LIMIT {}", CONNECTIONS_LIMIT)));
        // Active first, then idle in transaction: the cap is only defensible while
        // the rows it drops are the ones nobody was looking for.
        let active = sql.find("WHEN a.state = 'active' THEN 0").unwrap();
        let idle_in_transaction = sql
            .find("WHEN a.state LIKE 'idle in transaction%' THEN 1")
            .unwrap();
        assert!(active < idle_in_transaction);
    }
}
