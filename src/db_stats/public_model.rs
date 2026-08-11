//! The wire shape of a database's statistics.
//!
//! Deliberately shared by `GET /api/Stats` and the `db_stats` MCP tool rather
//! than declared once per consumer: the operator looking at the admin page and the
//! agent asking for table sizes must never be able to read two different numbers
//! for the same thing, and a second copy of a model this wide would drift on the
//! first field anyone added.
//!
//! That is why `MyHttpObjectStructure` is derived here, in the statistics module,
//! instead of in the controller next to the route the way the other endpoints do
//! it. The alternative — a plain-serde model here and an http-flavoured mirror in
//! the controller — is exactly the duplication being avoided.
//!
//! Two conventions worth knowing before adding a field:
//!
//! - **No `///` doc comments on fields.** The `MyHttpObjectStructure`
//!   proc-macro panics on them; use `//`.
//! - **Optional means "not known", never "zero".** Every number here is an
//!   `Option` because the catalog genuinely cannot answer some of these questions
//!   on some servers and some accounts — see [`crate::postgres::row_reader`].

use my_http_server::macros::*;
use serde::{Deserialize, Serialize};

use rust_extensions::date_time::DateTimeAsMicroseconds;

use super::{
    ActivityStats, DbHealth, DbHealthRates, DbStatsSnapshot, LongRunningQuery, Section,
    ServerCapabilities, TableStats, TablesStats, TopStatement, TopStatements,
};

fn as_rfc3339(value: Option<DateTimeAsMicroseconds>) -> Option<String> {
    value.map(|value| value.to_rfc3339())
}

/// `pg_stat_statements.queryid` is a signed 64-bit hash, routinely larger than the
/// 2^53 a JSON number survives in a JavaScript reader. It goes on the wire as a
/// string so it is still the same id after a round trip.
fn query_id_as_string(value: Option<i64>) -> Option<String> {
    value.map(|value| value.to_string())
}

#[derive(Serialize, Deserialize, Debug, Clone, MyHttpObjectStructure)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfoModel {
    // "pending" | "ready" | "unavailable"
    pub state: String,
    pub reason: Option<String>,
    pub version: Option<String>,
    pub database_name: Option<String>,
    pub user_name: Option<String>,
    pub is_superuser: Option<bool>,
    // Member of pg_monitor / pg_read_all_stats, or superuser. When false, the
    // activity counts undercount and other users' query texts are missing.
    pub can_read_all_stats: Option<bool>,
    pub has_pg_stat_statements: Option<bool>,
    pub max_connections: Option<i64>,
}

impl ServerInfoModel {
    fn new(section: &Section<ServerCapabilities>) -> Self {
        let data = section.data();

        Self {
            state: section.state_str().to_string(),
            reason: section.reason(),
            version: data.map(|d| d.server_version.clone()),
            database_name: data.map(|d| d.database_name.clone()),
            user_name: data.map(|d| d.user_name.clone()),
            is_superuser: data.map(|d| d.is_superuser),
            can_read_all_stats: data.map(|d| d.sees_all_stats()),
            has_pg_stat_statements: data.map(|d| d.has_pg_stat_statements),
            max_connections: data.map(|d| d.max_connections as i64),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, MyHttpObjectStructure)]
#[serde(rename_all = "camelCase")]
pub struct LongQueryModel {
    pub pid: Option<i64>,
    pub user_name: Option<String>,
    pub application_name: Option<String>,
    // The backend's own state ("active"), not the section's.
    pub backend_state: Option<String>,
    // "Lock: transactionid", "IO: DataFileRead", … null when not waiting.
    pub wait: Option<String>,
    pub running_secs: Option<f64>,
    // null when this account may not read another user's query text.
    pub query: Option<String>,
}

impl LongQueryModel {
    fn new(src: &LongRunningQuery) -> Self {
        Self {
            pid: src.pid.map(|pid| pid as i64),
            user_name: src.user_name.clone(),
            application_name: src.application_name.clone(),
            backend_state: src.state.clone(),
            wait: src.wait.clone(),
            running_secs: src.running_secs,
            query: src.query.clone(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, MyHttpObjectStructure)]
#[serde(rename_all = "camelCase")]
pub struct ActivityModel {
    pub state: String,
    pub reason: Option<String>,
    // Client backends across the whole cluster — what competes for maxConnections.
    pub total_client_backends: Option<i64>,
    pub in_this_db: Option<i64>,
    pub active: Option<i64>,
    pub idle: Option<i64>,
    pub idle_in_transaction: Option<i64>,
    pub waiting: Option<i64>,
    // Backends whose state this account may not read. Non-zero means the four
    // counts above are undercounts.
    pub state_unknown: Option<i64>,
    pub max_connections: Option<i64>,
    // Longest-running active statements on THIS database only, longest first.
    pub longest: Vec<LongQueryModel>,
}

impl ActivityModel {
    fn new(section: &Section<ActivityStats>) -> Self {
        let data = section.data();

        Self {
            state: section.state_str().to_string(),
            reason: section.reason(),
            total_client_backends: data.and_then(|d| d.total_client_backends),
            in_this_db: data.and_then(|d| d.in_this_db),
            active: data.and_then(|d| d.active),
            idle: data.and_then(|d| d.idle),
            idle_in_transaction: data.and_then(|d| d.idle_in_transaction),
            waiting: data.and_then(|d| d.waiting),
            state_unknown: data.and_then(|d| d.state_unknown),
            max_connections: data.and_then(|d| d.max_connections),
            longest: data
                .map(|d| d.longest.iter().map(LongQueryModel::new).collect())
                .unwrap_or_default(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, MyHttpObjectStructure)]
#[serde(rename_all = "camelCase")]
pub struct HealthRatesModel {
    pub window_secs: f64,
    pub commits_per_sec: Option<f64>,
    pub rollbacks_per_sec: Option<f64>,
    pub rows_written_per_sec: Option<f64>,
    pub blks_read_per_sec: Option<f64>,
    // Over the window, not since the stats reset.
    pub cache_hit_ratio: Option<f64>,
    // Backend-seconds of execution per wall-clock second: the closest thing to a
    // CPU figure Postgres exposes. 0.2 = executing 20% of the time, 3.0 = three
    // backends busy on average. Not a share of the host's CPU — it counts I/O and
    // lock waits as busy, and sees nothing outside this database. null on servers
    // older than 14, which have no active_time column.
    pub busy_backends: Option<f64>,
}

impl HealthRatesModel {
    fn new(src: &DbHealthRates) -> Self {
        Self {
            window_secs: src.window_secs,
            commits_per_sec: src.commits_per_sec,
            rollbacks_per_sec: src.rollbacks_per_sec,
            rows_written_per_sec: src.rows_written_per_sec,
            blks_read_per_sec: src.blks_read_per_sec,
            cache_hit_ratio: src.cache_hit_ratio,
            busy_backends: src.busy_backends,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, MyHttpObjectStructure)]
#[serde(rename_all = "camelCase")]
pub struct HealthModel {
    pub state: String,
    pub reason: Option<String>,
    pub db_size_bytes: Option<i64>,
    pub num_backends: Option<i64>,
    pub commits: Option<i64>,
    pub rollbacks: Option<i64>,
    pub deadlocks: Option<i64>,
    pub temp_files: Option<i64>,
    pub temp_bytes: Option<i64>,
    pub tup_returned: Option<i64>,
    pub tup_fetched: Option<i64>,
    pub rows_written: Option<i64>,
    pub lifetime_cache_hit_ratio: Option<f64>,
    pub stats_reset: Option<String>,
    pub active_time_ms: Option<f64>,
    pub session_time_ms: Option<f64>,
    // null on the first tick after start-up, and again after a pg_stat_reset().
    pub rates: Option<HealthRatesModel>,
}

impl HealthModel {
    fn new(section: &Section<DbHealth>) -> Self {
        let data = section.data();

        Self {
            state: section.state_str().to_string(),
            reason: section.reason(),
            db_size_bytes: data.and_then(|d| d.db_size_bytes),
            num_backends: data.and_then(|d| d.num_backends),
            commits: data.and_then(|d| d.commits),
            rollbacks: data.and_then(|d| d.rollbacks),
            deadlocks: data.and_then(|d| d.deadlocks),
            temp_files: data.and_then(|d| d.temp_files),
            temp_bytes: data.and_then(|d| d.temp_bytes),
            tup_returned: data.and_then(|d| d.tup_returned),
            tup_fetched: data.and_then(|d| d.tup_fetched),
            rows_written: data.and_then(|d| d.rows_written),
            lifetime_cache_hit_ratio: data.and_then(|d| d.lifetime_cache_hit_ratio),
            stats_reset: data.and_then(|d| d.stats_reset.clone()),
            active_time_ms: data.and_then(|d| d.active_time_ms),
            session_time_ms: data.and_then(|d| d.session_time_ms),
            rates: data
                .and_then(|d| d.rates.as_ref())
                .map(HealthRatesModel::new),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, MyHttpObjectStructure)]
#[serde(rename_all = "camelCase")]
pub struct StatementModel {
    // pg_stat_statements.queryid, as a string — see query_id_as_string.
    pub query_id: Option<String>,
    pub calls: Option<i64>,
    pub total_exec_ms: Option<f64>,
    pub mean_exec_ms: Option<f64>,
    pub rows_returned: Option<i64>,
    pub blks_hit: Option<i64>,
    pub blks_read: Option<i64>,
    // Normalized text, constants replaced by $1, $2, …
    pub query: Option<String>,
    pub delta_calls: Option<i64>,
    pub delta_exec_ms: Option<f64>,
    // Milliseconds of execution per wall-clock second since the previous slow
    // tick. 1000 means this statement alone kept a backend busy continuously.
    pub exec_ms_per_sec: Option<f64>,
}

impl StatementModel {
    fn new(src: &TopStatement) -> Self {
        Self {
            query_id: query_id_as_string(src.query_id),
            calls: src.calls,
            total_exec_ms: src.total_exec_ms,
            mean_exec_ms: src.mean_exec_ms,
            rows_returned: src.rows_returned,
            blks_hit: src.blks_hit,
            blks_read: src.blks_read,
            query: src.query.clone(),
            delta_calls: src.delta_calls,
            delta_exec_ms: src.delta_exec_ms,
            exec_ms_per_sec: src.exec_ms_per_sec,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, MyHttpObjectStructure)]
#[serde(rename_all = "camelCase")]
pub struct LoadModel {
    pub state: String,
    // When unavailable, says which piece is missing — usually the
    // pg_stat_statements extension.
    pub reason: Option<String>,
    // False when the account is not a member of pg_monitor/pg_read_all_stats: the
    // list then covers only the statements this connection's own role executed.
    pub sees_all_statements: Option<bool>,
    // Ordered by execution time in the last window, then by lifetime total.
    pub items: Vec<StatementModel>,
}

impl LoadModel {
    fn new(section: &Section<TopStatements>) -> Self {
        let data = section.data();

        Self {
            state: section.state_str().to_string(),
            reason: section.reason(),
            sees_all_statements: data.map(|d| d.sees_all_statements),
            items: data
                .map(|d| d.items.iter().map(StatementModel::new).collect())
                .unwrap_or_default(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, MyHttpObjectStructure)]
#[serde(rename_all = "camelCase")]
pub struct TableModel {
    pub schema: Option<String>,
    pub name: Option<String>,
    // Heap + indexes + TOAST.
    pub total_bytes: Option<i64>,
    pub table_bytes: Option<i64>,
    pub index_bytes: Option<i64>,
    // Planner estimates, not count(*).
    pub live_tuples: Option<i64>,
    pub dead_tuples: Option<i64>,
    pub seq_scans: Option<i64>,
    // null on a table no index has ever been used on.
    pub idx_scans: Option<i64>,
    pub last_vacuum: Option<String>,
    pub last_analyze: Option<String>,
}

impl TableModel {
    fn new(src: &TableStats) -> Self {
        Self {
            schema: src.schema_name.clone(),
            name: src.table_name.clone(),
            total_bytes: src.total_bytes,
            table_bytes: src.table_bytes,
            index_bytes: src.index_bytes,
            live_tuples: src.live_tuples,
            dead_tuples: src.dead_tuples,
            seq_scans: src.seq_scans,
            idx_scans: src.idx_scans,
            last_vacuum: src.last_vacuum.clone(),
            last_analyze: src.last_analyze.clone(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, MyHttpObjectStructure)]
#[serde(rename_all = "camelCase")]
pub struct TablesModel {
    pub state: String,
    pub reason: Option<String>,
    // Every table in the database, so a truncated list can say "top 25 of 812".
    pub table_count: Option<i64>,
    // Largest first, by heap + indexes + TOAST.
    pub items: Vec<TableModel>,
}

impl TablesModel {
    fn new(section: &Section<TablesStats>) -> Self {
        let data = section.data();

        Self {
            state: section.state_str().to_string(),
            reason: section.reason(),
            table_count: data.and_then(|d| d.table_count),
            items: data
                .map(|d| d.items.iter().map(TableModel::new).collect())
                .unwrap_or_default(),
        }
    }
}

/// One database's statistics, with no mention of which mount it came from.
///
/// That omission is the reason this type exists separately from
/// [`DatabaseStatsModel`]: it is what the MCP tool returns, and an endpoint must
/// never hint at the other configured databases.
#[derive(Serialize, Deserialize, Debug, Clone, MyHttpObjectStructure)]
#[serde(rename_all = "camelCase")]
pub struct DbStatsModel {
    // When the live sections (activity, health) were last refreshed; null before
    // the first tick.
    pub collected_at: Option<String>,
    // When the slower sections (server, load, tables) were last refreshed.
    pub slow_collected_at: Option<String>,
    // Set when the database itself could not be reached, rather than one section
    // being unavailable.
    pub last_error: Option<String>,
    pub server: ServerInfoModel,
    pub activity: ActivityModel,
    pub health: HealthModel,
    pub load: LoadModel,
    pub tables: TablesModel,
}

impl DbStatsModel {
    pub fn new(src: &DbStatsSnapshot) -> Self {
        Self {
            collected_at: as_rfc3339(src.collected_at),
            slow_collected_at: as_rfc3339(src.slow_collected_at),
            last_error: src.last_error.clone(),
            server: ServerInfoModel::new(&src.server),
            activity: ActivityModel::new(&src.activity),
            health: HealthModel::new(&src.health),
            load: LoadModel::new(&src.statements),
            tables: TablesModel::new(&src.tables),
        }
    }
}

/// A database's statistics plus the mount that identifies it — the admin API's
/// row shape. The admin sees every mount; an MCP endpoint sees only
/// [`DbStatsModel`].
#[derive(Serialize, Deserialize, Debug, Clone, MyHttpObjectStructure)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseStatsModel {
    // MCP mount path, e.g. "/mcp".
    pub path: String,
    pub description: String,
    pub stats: DbStatsModel,
}

/// State of the metrics history file — server-wide, not per database.
///
/// Reported alongside the live numbers because the two can disagree: every card on
/// the page can be updating while nothing at all is being recorded, and a full disk
/// or a read-only home directory looks exactly like a healthy server otherwise.
#[derive(Serialize, Deserialize, Debug, Clone, MyHttpObjectStructure)]
#[serde(rename_all = "camelCase")]
pub struct HistoryInfoModel {
    pub enabled: bool,
    pub path: String,
    pub retention_days: i64,
    // Why history is unavailable for this run — the file could not be opened.
    pub open_error: Option<String>,
    // The last failure from a history write or a retention sweep, cleared by the
    // next tick that succeeds.
    pub write_error: Option<String>,
}

impl HistoryInfoModel {
    fn new(app: &crate::app::AppContext) -> Self {
        Self {
            enabled: app.metrics.is_enabled(),
            path: app.metrics.path().to_string(),
            retention_days: (super::RETENTION.as_secs() / (24 * 60 * 60)) as i64,
            open_error: app.metrics.open_error().map(|err| err.to_string()),
            write_error: app.metrics_write_error.get(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, MyHttpObjectStructure)]
#[serde(rename_all = "camelCase")]
pub struct StatsModel {
    pub history: HistoryInfoModel,
    pub databases: Vec<DatabaseStatsModel>,
}

impl StatsModel {
    pub fn new(app: &crate::app::AppContext) -> Self {
        Self {
            history: HistoryInfoModel::new(app),
            databases: app
                .databases
                .iter()
                .map(|db| DatabaseStatsModel {
                    path: db.path.as_str().to_string(),
                    description: db.description.clone(),
                    stats: DbStatsModel::new(db.stats.get().as_ref()),
                })
                .collect(),
        }
    }
}
