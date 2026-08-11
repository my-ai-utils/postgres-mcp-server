use my_http_server::macros::*;
use serde::{Deserialize, Serialize};

use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::db_stats::{HistoryRow, LoadSample, StatementLoadSample, TableSizeSample};

/// Sections of the history, mirroring the `db_stats` MCP tool's `section` argument
/// so the two surfaces use the same vocabulary.
pub const SECTION_LOAD: &str = "load";
pub const SECTION_TABLES: &str = "tables";
pub const SECTION_STATEMENTS: &str = "statements";

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
        description = "Which series to return: \"load\" (the 5-second load samples), \"tables\" (hourly table sizes) or \"statements\" (hourly statement costs). Defaults to \"load\".",
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

/// One point of the load series. Flattened rather than nested so a chart can read
/// `at` and the value it wants from the same object.
#[derive(Serialize, Deserialize, Debug, Clone, MyHttpObjectStructure)]
#[serde(rename_all = "camelCase")]
pub struct LoadPointModel {
    pub at: String,
    // Backend-seconds of execution per wall-clock second — the closest proxy to
    // CPU that Postgres exposes. Null on servers older than 14.
    pub busy_backends: Option<f64>,
    pub commits_per_sec: Option<f64>,
    pub rollbacks_per_sec: Option<f64>,
    pub rows_written_per_sec: Option<f64>,
    pub blks_read_per_sec: Option<f64>,
    pub cache_hit_ratio: Option<f64>,
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
            busy_backends: src.value.busy_backends,
            commits_per_sec: src.value.commits_per_sec,
            rollbacks_per_sec: src.value.rollbacks_per_sec,
            rows_written_per_sec: src.value.rows_written_per_sec,
            blks_read_per_sec: src.value.blks_read_per_sec,
            cache_hit_ratio: src.value.cache_hit_ratio,
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

/// Exactly one of the three series is populated per request — the one named by
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
}
