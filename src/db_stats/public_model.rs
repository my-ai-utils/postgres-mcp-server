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
    ActivityStats, DbHealth, DbHealthRates, DbStatsSnapshot, DiskIo, LongRunningQuery, Section,
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
    // Whether the server times its I/O. Off by default; without it every I/O timing
    // figure is null rather than zero.
    pub track_io_timing: Option<bool>,
    // pg_stat_statements is in pg_available_extensions — the contrib package is
    // installed, so the library is on disk. Setting shared_preload_libraries to a
    // library that is NOT on disk stops Postgres from starting, so this gates the
    // offer to do it.
    pub pg_stat_statements_available: Option<bool>,
    // Already in shared_preload_libraries: CREATE EXTENSION alone is enough, and no
    // restart is needed.
    pub pg_stat_statements_preloaded: Option<bool>,
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
            track_io_timing: data.map(|d| d.track_io_timing),
            pg_stat_statements_available: data.map(|d| d.pg_stat_statements_available),
            pg_stat_statements_preloaded: data.map(|d| d.pg_stat_statements_preloaded),
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
    // Milliseconds per wall-clock second the database spent blocked on reads /
    // writes. 1000 means a full second of every second was pure waiting. null when
    // track_io_timing is off.
    pub io_read_ms_per_sec: Option<f64>,
    pub io_write_ms_per_sec: Option<f64>,
    // I/O wait as a share of execution time: 0.8 means four fifths of the time
    // backends were "executing", they were waiting on the disk.
    pub io_share_of_active: Option<f64>,
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
            io_read_ms_per_sec: src.io_read_ms_per_sec,
            io_write_ms_per_sec: src.io_write_ms_per_sec,
            io_share_of_active: src.io_share_of_active,
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
    // Lifetime milliseconds waited on reads/writes. null unless track_io_timing is
    // on — Postgres reports a hard zero when it is off, which would read as "this
    // database never touches the disk".
    pub blk_read_time_ms: Option<f64>,
    pub blk_write_time_ms: Option<f64>,
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
            blk_read_time_ms: data.and_then(|d| d.blk_read_time_ms),
            blk_write_time_ms: data.and_then(|d| d.blk_write_time_ms),
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

#[derive(Serialize, Deserialize, Debug, Clone, MyHttpObjectStructure)]
#[serde(rename_all = "camelCase")]
pub struct TableIoModel {
    pub schema: Option<String>,
    pub name: Option<String>,
    // Heap + index + TOAST blocks not served from shared buffers, since the reset.
    pub total_read_blocks: Option<i64>,
    pub heap_read_blocks: Option<i64>,
    pub index_read_blocks: Option<i64>,
    pub toast_read_blocks: Option<i64>,
    // Share of this table's block accesses shared buffers satisfied — what separates
    // "big table, fully cached" from "big table, read off disk".
    pub cache_hit_ratio: Option<f64>,
    pub delta_read_blocks: Option<i64>,
    pub delta_read_bytes: Option<i64>,
    pub read_bytes_per_sec: Option<f64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, MyHttpObjectStructure)]
#[serde(rename_all = "camelCase")]
pub struct WriteIoModel {
    // Cluster-wide, unlike the per-table reads above.
    pub wal_bytes: Option<i64>,
    pub wal_records: Option<i64>,
    // Whole 8 kB pages copied into WAL on the first write to them after a
    // checkpoint. A large share of WAL means checkpoints are too frequent for the
    // write rate.
    pub wal_full_page_images: Option<i64>,
    pub wal_bytes_per_sec: Option<f64>,
    pub checkpoints_timed: Option<i64>,
    // Checkpoints forced early by max_wal_size. Many of these relative to timed ones
    // means max_wal_size is too small, and each one restarts the full-page-image cost.
    pub checkpoints_requested: Option<i64>,
    pub buffers_written_by_checkpointer: Option<i64>,
    pub buffers_written_by_bgwriter: Option<i64>,
    // Buffers a query backend had to write itself because no clean buffer was free —
    // a query stalling to do the writer's job. null on Postgres 17+, where the
    // counter moved to pg_stat_io.
    pub buffers_written_by_backends: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, MyHttpObjectStructure)]
#[serde(rename_all = "camelCase")]
pub struct DiskIoModel {
    pub state: String,
    pub reason: Option<String>,
    // False when track_io_timing is off. Every *timing* figure is then null rather
    // than zero, because Postgres reports a hard zero for "nobody measured".
    pub io_timing_enabled: Option<bool>,
    // Tables ranked by blocks read since the previous tick, falling back to lifetime.
    pub tables: Vec<TableIoModel>,
    pub writes: Option<WriteIoModel>,
    // Why the write half is missing, when it is.
    pub writes_unavailable: Option<String>,
}

impl DiskIoModel {
    fn new(section: &Section<DiskIo>) -> Self {
        let data = section.data();

        Self {
            state: section.state_str().to_string(),
            reason: section.reason(),
            io_timing_enabled: data.map(|d| d.io_timing_enabled),
            tables: data
                .map(|d| {
                    d.tables
                        .iter()
                        .map(|src| TableIoModel {
                            schema: src.schema_name.clone(),
                            name: src.table_name.clone(),
                            total_read_blocks: src.total_read_blocks,
                            heap_read_blocks: src.heap_read_blocks,
                            index_read_blocks: src.index_read_blocks,
                            toast_read_blocks: src.toast_read_blocks,
                            cache_hit_ratio: src.cache_hit_ratio,
                            delta_read_blocks: src.delta_read_blocks,
                            delta_read_bytes: src.delta_read_bytes,
                            read_bytes_per_sec: src.read_bytes_per_sec,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            writes: data.and_then(|d| d.writes.as_ref()).map(|src| WriteIoModel {
                wal_bytes: src.wal_bytes,
                wal_records: src.wal_records,
                wal_full_page_images: src.wal_full_page_images,
                wal_bytes_per_sec: src.wal_bytes_per_sec,
                checkpoints_timed: src.checkpoints_timed,
                checkpoints_requested: src.checkpoints_requested,
                buffers_written_by_checkpointer: src.buffers_written_by_checkpointer,
                buffers_written_by_bgwriter: src.buffers_written_by_bgwriter,
                buffers_written_by_backends: src.buffers_written_by_backends,
            }),
            writes_unavailable: data.and_then(|d| d.writes_unavailable.clone()),
        }
    }
}

/// One minute of traffic: how many statements ran, how long they took on average,
/// and the longest one.
#[derive(Serialize, Deserialize, Debug, Clone, MyHttpObjectStructure)]
#[serde(rename_all = "camelCase")]
pub struct ThroughputModel {
    // The window these cover. Nominally 60s; reported because a tick that ran late
    // makes "per minute" a lie the reader cannot otherwise see.
    pub window_secs: f64,
    // Statements that completed in the window, across the WHOLE database — not just
    // the ones this server's agent ran. From pg_stat_statements, so exact.
    pub calls: Option<i64>,
    pub calls_per_sec: Option<f64>,
    // Total execution time over calls. Exact for the window.
    pub avg_exec_ms: Option<f64>,
    pub total_exec_ms: Option<f64>,
    // Longest execution OBSERVED by the 5-second sampler. A floor, not a maximum: a
    // statement that starts and finishes between two samples is never seen. The
    // misses are the fast ones.
    pub longest_secs: Option<f64>,
    pub longest_query: Option<String>,
    // A new lifetime maximum set inside the window — exact when present, absent when
    // no record was broken. Never a guess.
    pub slowest_finished_ms: Option<f64>,
    pub slowest_finished_query: Option<String>,
}

impl ThroughputModel {
    fn new(src: &super::MinuteThroughput) -> Self {
        Self {
            window_secs: src.window_secs,
            calls: src.calls,
            calls_per_sec: src.calls_per_sec,
            avg_exec_ms: src.avg_exec_ms,
            total_exec_ms: src.total_exec_ms,
            longest_secs: src.longest_secs,
            longest_query: src.longest_query.clone(),
            slowest_finished_ms: src.slowest_finished_ms,
            slowest_finished_query: src.slowest_finished_query.clone(),
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
    pub disk_io: DiskIoModel,
    // The last completed minute. null until two slow ticks have run — a minute needs
    // two readings to exist at all.
    pub throughput: Option<ThroughputModel>,
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
            disk_io: DiskIoModel::new(&src.disk_io),
            throughput: src.throughput.as_ref().map(ThroughputModel::new),
        }
    }
}

/// Which Postgres server a mount talks to.
///
/// Derived from the connection string, and carrying **only** host, port and the
/// SSH target — never the credentials, since this goes to an unauthenticated API.
/// See [`crate::settings::ServerEndpoint`].
#[derive(Serialize, Deserialize, Debug, Clone, MyHttpObjectStructure)]
#[serde(rename_all = "camelCase")]
pub struct ServerEndpointModel {
    // Stable id: mounts sharing it are on the same server. The UI groups and
    // switches by this.
    pub key: String,
    // What the operator sees, e.g. "db.internal" or "db:6432 (via ssh deploy@h:22)".
    pub label: String,
    pub host: String,
    pub port: i64,
    // The database this mount opens, from the connection string.
    pub database: Option<String>,
    // Server + database. Two mounts sharing this open the SAME database, and so read
    // the very same pg_stat_database counters — anything that treats them as two
    // independent sources counts that database twice. `null` when the connection
    // string does not name a database (libpq then falls back to the user name), in
    // which case sameness cannot be proven from configuration and callers must treat
    // the mounts as distinct rather than guess.
    pub database_key: Option<String>,
}

impl ServerEndpointModel {
    fn new(src: &crate::settings::ServerEndpoint) -> Self {
        Self {
            key: src.key(),
            label: src.label(),
            host: src.host.clone(),
            port: src.port as i64,
            database: src.dbname.clone(),
            database_key: src.database_key(),
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
    // Which server this database lives on. Several mounts commonly share one.
    pub server: ServerEndpointModel,
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
                    server: ServerEndpointModel::new(&db.server),
                    stats: DbStatsModel::new(db.stats.get().as_ref()),
                })
                .collect(),
        }
    }
}
