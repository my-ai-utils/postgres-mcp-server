//! Mirror of `GET /api/Stats/History`.
//!
//! Only the `load` series is mirrored: it is the one the UI plots. `tables`,
//! `statements` and `longest` are recorded and served, but they are read through
//! the API and the MCP tool rather than drawn here, so mirroring them would be
//! three more structs to keep in step for nothing.

use serde::Deserialize;

/// One 5-second load sample of **one database**.
#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LoadPoint {
    /// RFC 3339, as the server renders it.
    pub at: String,
    /// The same instant as epoch milliseconds — the server sends this so the chart
    /// can place points and measure gaps without a date library in the bundle.
    pub at_unix_ms: i64,
    /// Backend-seconds of execution per wall-clock second, for this database.
    ///
    /// `None` before Postgres 14 (no `active_time` column) and on the first sample
    /// after a `pg_stat_reset()`, when there is no comparable previous reading.
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

/// The response.
///
/// The `tables`, `statements` and `longest` arrays the server also sends are simply
/// not declared — serde ignores unknown fields — because this type is only ever
/// fetched with `section=load`, where they are always empty. The strictness that
/// matters is the other direction: every field named here must arrive, so a rename
/// on the server fails the fetch instead of quietly plotting nothing.
#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LoadHistory {
    pub path: String,
    pub section: String,
    pub from: String,
    pub to: String,
    /// Set when history is disabled or the read failed; `load` is then empty.
    pub error: Option<String>,
    /// Oldest first.
    pub load: Vec<LoadPoint>,
}
