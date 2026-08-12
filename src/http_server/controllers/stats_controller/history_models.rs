use my_http_server::macros::*;
use serde::{Deserialize, Serialize};

use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::db_stats::{
    HistoryRow, LoadSample, LongestQuerySample, MinuteThroughputSample, StatementLoadSample,
    TableSizeSample,
};

/// Sections of the history, mirroring the `db_stats` MCP tool's `section` argument
/// so the two surfaces use the same vocabulary.
pub const SECTION_LOAD: &str = "load";
pub const SECTION_TABLES: &str = "tables";
pub const SECTION_STATEMENTS: &str = "statements";
pub const SECTION_LONGEST: &str = "longest";
pub const SECTION_MINUTES: &str = "minutes";

/// Hard ceiling on the window. Retention is three days, so a longer request could
/// only ever return the same rows; the cap keeps a hand-typed `hours=100000` from
/// scanning the whole file.
pub const MAX_HOURS: i64 = 72;

pub const DEFAULT_HOURS: i64 = 3;

#[derive(MyHttpInput)]
pub struct HistoryInput {
    #[http_query(
        name = "path",
        description = "MCP mount path of the database, e.g. \"/mcp\", as returned by GET /api/Settings."
    )]
    pub path: String,

    #[http_query(
        name = "hours",
        description = "How far back to read, in hours. Capped at 72 — history is kept for 3 days.",
        default = 3
    )]
    pub hours: Option<i64>,

    #[http_query(
        name = "section",
        description = "Which series to return: \"load\" (the 5-second load samples), \"minutes\" (per-minute calls, average time and longest query), \"tables\" (hourly table sizes), \"statements\" (hourly statement costs) or \"longest\" (each hour's top-5 longest-running statements). Defaults to \"load\".",
        default = "load"
    )]
    pub section: Option<String>,
}

impl HistoryInput {
    /// Clamped rather than rejected: a window wider than retention is a reasonable
    /// thing to ask for and the honest answer is "here is everything there is",
    /// not a 400.
    pub fn window(&self) -> (DateTimeAsMicroseconds, DateTimeAsMicroseconds) {
        let hours = self.hours.unwrap_or(DEFAULT_HOURS).clamp(1, MAX_HOURS);

        let to = DateTimeAsMicroseconds::now();
        let from = to.sub(std::time::Duration::from_secs(hours as u64 * 60 * 60));

        (from, to)
    }

    pub fn section(&self) -> &str {
        match self.section.as_deref() {
            Some(section) if !section.trim().is_empty() => section.trim(),
            _ => SECTION_LOAD,
        }
    }
}

/// One point of the load series — one 5-second sample of one database.
#[derive(Serialize, Deserialize, Debug, Clone, MyHttpObjectStructure)]
#[serde(rename_all = "camelCase")]
pub struct LoadPointModel {
    pub at: String,
    // The same instant as epoch milliseconds. Sent alongside `at` because a chart
    // has to do arithmetic on time — place a point on an axis, measure the gap to
    // its neighbour to decide whether the line breaks — and parsing RFC 3339 in the
    // wasm client would mean a date library in the bundle for one subtraction.
    pub at_unix_ms: i64,
    // Backend-seconds of execution per wall-clock second, for THIS database.
    // Not a CPU figure: a backend waiting on disk or a lock still counts as
    // executing. Null on servers older than 14, which have no active_time column.
    pub busy_backends: Option<f64>,
    pub commits_per_sec: Option<f64>,
    pub rollbacks_per_sec: Option<f64>,
    pub rows_written_per_sec: Option<f64>,
    pub blks_read_per_sec: Option<f64>,
    pub cache_hit_ratio: Option<f64>,
    // Milliseconds per second lost waiting on disk. null when track_io_timing is off.
    pub io_read_ms_per_sec: Option<f64>,
    pub io_write_ms_per_sec: Option<f64>,
    pub db_size_bytes: Option<i64>,
    pub backends_total: Option<i64>,
    pub backends_active: Option<i64>,
    pub backends_idle: Option<i64>,
    pub backends_idle_in_transaction: Option<i64>,
    pub backends_waiting: Option<i64>,
}

impl LoadPointModel {
    fn new(src: HistoryRow<LoadSample>) -> Self {
        Self {
            at: src.at.to_rfc3339(),
            at_unix_ms: src.at.unix_microseconds / 1_000,
            busy_backends: src.value.busy_backends,
            commits_per_sec: src.value.commits_per_sec,
            rollbacks_per_sec: src.value.rollbacks_per_sec,
            rows_written_per_sec: src.value.rows_written_per_sec,
            blks_read_per_sec: src.value.blks_read_per_sec,
            cache_hit_ratio: src.value.cache_hit_ratio,
            io_read_ms_per_sec: src.value.io_read_ms_per_sec,
            io_write_ms_per_sec: src.value.io_write_ms_per_sec,
            db_size_bytes: src.value.db_size_bytes,
            backends_total: src.value.backends_total,
            backends_active: src.value.backends_active,
            backends_idle: src.value.backends_idle,
            backends_idle_in_transaction: src.value.backends_idle_in_transaction,
            backends_waiting: src.value.backends_waiting,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, MyHttpObjectStructure)]
#[serde(rename_all = "camelCase")]
pub struct TableSizePointModel {
    pub schema: Option<String>,
    pub name: Option<String>,
    pub total_bytes: Option<i64>,
    pub table_bytes: Option<i64>,
    pub index_bytes: Option<i64>,
    pub live_tuples: Option<i64>,
    pub dead_tuples: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, MyHttpObjectStructure)]
#[serde(rename_all = "camelCase")]
pub struct TableSizeSnapshotModel {
    pub at: String,
    pub items: Vec<TableSizePointModel>,
}

impl TableSizeSnapshotModel {
    fn new(src: HistoryRow<Vec<TableSizeSample>>) -> Self {
        Self {
            at: src.at.to_rfc3339(),
            items: src
                .value
                .into_iter()
                .map(|item| TableSizePointModel {
                    schema: item.schema,
                    name: item.name,
                    total_bytes: item.total_bytes,
                    table_bytes: item.table_bytes,
                    index_bytes: item.index_bytes,
                    live_tuples: item.live_tuples,
                    dead_tuples: item.dead_tuples,
                })
                .collect(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, MyHttpObjectStructure)]
#[serde(rename_all = "camelCase")]
pub struct StatementPointModel {
    pub query_id: Option<String>,
    pub query: Option<String>,
    pub calls: Option<i64>,
    pub mean_exec_ms: Option<f64>,
    pub delta_exec_ms: Option<f64>,
    pub exec_ms_per_sec: Option<f64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, MyHttpObjectStructure)]
#[serde(rename_all = "camelCase")]
pub struct StatementSnapshotModel {
    pub at: String,
    pub items: Vec<StatementPointModel>,
}

impl StatementSnapshotModel {
    fn new(src: HistoryRow<Vec<StatementLoadSample>>) -> Self {
        Self {
            at: src.at.to_rfc3339(),
            items: src
                .value
                .into_iter()
                .map(|item| StatementPointModel {
                    query_id: item.query_id,
                    query: item.query,
                    calls: item.calls,
                    mean_exec_ms: item.mean_exec_ms,
                    delta_exec_ms: item.delta_exec_ms,
                    exec_ms_per_sec: item.exec_ms_per_sec,
                })
                .collect(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, MyHttpObjectStructure)]
#[serde(rename_all = "camelCase")]
pub struct LongestQueryPointModel {
    pub pid: Option<i64>,
    pub query_start: Option<String>,
    pub user_name: Option<String>,
    pub application_name: Option<String>,
    pub wait: Option<String>,
    // The longest any 5-second tick saw this execution run for.
    pub running_secs: f64,
    pub query: Option<String>,
}

/// One hour's top-5 longest-running statements, longest first.
#[derive(Serialize, Deserialize, Debug, Clone, MyHttpObjectStructure)]
#[serde(rename_all = "camelCase")]
pub struct LongestQuerySnapshotModel {
    // End of the hour these were observed in.
    pub at: String,
    pub items: Vec<LongestQueryPointModel>,
}

impl LongestQuerySnapshotModel {
    fn new(src: HistoryRow<Vec<LongestQuerySample>>) -> Self {
        Self {
            at: src.at.to_rfc3339(),
            items: src
                .value
                .into_iter()
                .map(|item| LongestQueryPointModel {
                    pid: item.pid.map(|pid| pid as i64),
                    query_start: item.query_start,
                    user_name: item.user_name,
                    application_name: item.application_name,
                    wait: item.wait,
                    running_secs: item.running_secs,
                    query: item.query,
                })
                .collect(),
        }
    }
}

/// One minute of traffic.
#[derive(Serialize, Deserialize, Debug, Clone, MyHttpObjectStructure)]
#[serde(rename_all = "camelCase")]
pub struct MinutePointModel {
    pub at: String,
    // The same instant as epoch milliseconds — a chart has to do arithmetic on time,
    // and parsing RFC 3339 in the wasm client would mean a date library for one
    // subtraction.
    pub at_unix_ms: i64,
    // Nominally 60s. Reported because a tick that ran late makes "per minute" a lie
    // the reader cannot otherwise see.
    pub window_secs: f64,
    // Statements completed in the window, across the whole database. Exact.
    pub calls: Option<i64>,
    pub calls_per_sec: Option<f64>,
    pub avg_exec_ms: Option<f64>,
    pub total_exec_ms: Option<f64>,
    // Longest OBSERVED by the 5-second sampler — a floor, not a maximum.
    pub longest_secs: Option<f64>,
    pub longest_query: Option<String>,
    // A new lifetime maximum set inside the window: exact when present.
    pub slowest_finished_ms: Option<f64>,
    pub slowest_finished_query: Option<String>,
}

impl MinutePointModel {
    fn new(src: HistoryRow<MinuteThroughputSample>) -> Self {
        Self {
            at: src.at.to_rfc3339(),
            at_unix_ms: src.at.unix_microseconds / 1_000,
            window_secs: src.value.window_secs,
            calls: src.value.calls,
            calls_per_sec: src.value.calls_per_sec,
            avg_exec_ms: src.value.avg_exec_ms,
            total_exec_ms: src.value.total_exec_ms,
            longest_secs: src.value.longest_secs,
            longest_query: src.value.longest_query,
            slowest_finished_ms: src.value.slowest_finished_ms,
            slowest_finished_query: src.value.slowest_finished_query,
        }
    }
}

/// Exactly one of the series is populated per request — the one named by
/// `section`. They are separate fields rather than one polymorphic list so the
/// shape is statically known to both swagger and the UI mirror.
#[derive(Serialize, Deserialize, Debug, Clone, MyHttpObjectStructure)]
#[serde(rename_all = "camelCase")]
pub struct HistoryModel {
    pub path: String,
    pub section: String,
    pub from: String,
    pub to: String,
    // Set when history is disabled or the read failed; the series are then empty.
    pub error: Option<String>,
    // Oldest first, so a chart can plot straight through.
    pub load: Vec<LoadPointModel>,
    pub tables: Vec<TableSizeSnapshotModel>,
    pub statements: Vec<StatementSnapshotModel>,
    pub longest: Vec<LongestQuerySnapshotModel>,
    pub minutes: Vec<MinutePointModel>,
}

impl HistoryModel {
    pub fn empty(
        path: String,
        section: &str,
        from: DateTimeAsMicroseconds,
        to: DateTimeAsMicroseconds,
        error: Option<String>,
    ) -> Self {
        Self {
            path,
            section: section.to_string(),
            from: from.to_rfc3339(),
            to: to.to_rfc3339(),
            error,
            load: Vec::new(),
            tables: Vec::new(),
            statements: Vec::new(),
            longest: Vec::new(),
            minutes: Vec::new(),
        }
    }

    pub fn with_load(mut self, rows: Vec<HistoryRow<LoadSample>>) -> Self {
        self.load = rows.into_iter().map(LoadPointModel::new).collect();
        self
    }

    pub fn with_tables(mut self, rows: Vec<HistoryRow<Vec<TableSizeSample>>>) -> Self {
        self.tables = rows.into_iter().map(TableSizeSnapshotModel::new).collect();
        self
    }

    pub fn with_statements(mut self, rows: Vec<HistoryRow<Vec<StatementLoadSample>>>) -> Self {
        self.statements = rows
            .into_iter()
            .map(StatementSnapshotModel::new)
            .collect();
        self
    }

    pub fn with_minutes(mut self, rows: Vec<HistoryRow<MinuteThroughputSample>>) -> Self {
        self.minutes = rows.into_iter().map(MinutePointModel::new).collect();
        self
    }

    pub fn with_longest(mut self, rows: Vec<HistoryRow<Vec<LongestQuerySample>>>) -> Self {
        self.longest = rows
            .into_iter()
            .map(LongestQuerySnapshotModel::new)
            .collect();
        self
    }
}
