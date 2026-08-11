//! Mirrors of the server's `GET /api/Stats` models.
//!
//! Same rules as the rest of `crate::models` (see [`crate::api`]): plain serde,
//! declared strictly, no `#[serde(default)]` on anything the server always sends —
//! so a field renamed on the server surfaces as a failed fetch in the console
//! rather than as a page of zeroes that looks like a quiet database.
//!
//! The server serializes these with `rename_all = "camelCase"`, so the mirrors do
//! too rather than repeating a `rename` per field.
//!
//! Every number is an `Option` because the server means it: Postgres genuinely
//! cannot answer some of these on some versions and some accounts, and the
//! difference between "0" and "not known" is the whole point of the page. That is
//! what [`fmt`] renders as `—`.

use serde::Deserialize;

/// Formatting for values that may not exist. Kept in one place so a missing
/// number looks the same everywhere on the page.
pub mod fmt {
    /// What a value the database could not report looks like.
    pub const NONE: &str = "—";

    pub fn int(value: Option<i64>) -> String {
        match value {
            Some(value) => group(value),
            None => NONE.to_string(),
        }
    }

    /// Thousands separators, because these are read at a glance and
    /// `18446744` vs `1844674` is otherwise a coin toss.
    pub fn group(value: i64) -> String {
        let negative = value < 0;
        let digits = value.abs().to_string();

        let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);

        for (index, ch) in digits.chars().enumerate() {
            if index > 0 && (digits.len() - index) % 3 == 0 {
                out.push(' ');
            }
            out.push(ch);
        }

        if negative {
            return format!("-{}", out);
        }

        out
    }

    pub fn float(value: Option<f64>, decimals: usize) -> String {
        match value {
            Some(value) => format!("{:.*}", decimals, value),
            None => NONE.to_string(),
        }
    }

    /// `0.9912` -> `99.1%`.
    pub fn ratio(value: Option<f64>) -> String {
        match value {
            Some(value) => format!("{:.1}%", value * 100.0),
            None => NONE.to_string(),
        }
    }

    /// Binary units, the way Postgres' own `pg_size_pretty` reports them.
    pub fn bytes(value: Option<i64>) -> String {
        let Some(value) = value else {
            return NONE.to_string();
        };

        const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];

        let negative = value < 0;
        let mut size = value.abs() as f64;
        let mut unit = 0;

        while size >= 1024.0 && unit < UNITS.len() - 1 {
            size /= 1024.0;
            unit += 1;
        }

        let rendered = if unit == 0 {
            format!("{} {}", size as i64, UNITS[unit])
        } else if size >= 100.0 {
            format!("{:.0} {}", size, UNITS[unit])
        } else {
            format!("{:.1} {}", size, UNITS[unit])
        };

        if negative {
            return format!("-{}", rendered);
        }

        rendered
    }

    /// Milliseconds as the unit that keeps the number readable.
    pub fn millis(value: Option<f64>) -> String {
        let Some(value) = value else {
            return NONE.to_string();
        };

        if value >= 60_000.0 {
            return format!("{:.1} min", value / 60_000.0);
        }

        if value >= 1_000.0 {
            return format!("{:.2} s", value / 1_000.0);
        }

        if value >= 1.0 {
            return format!("{:.1} ms", value);
        }

        format!("{:.3} ms", value)
    }

    pub fn seconds(value: Option<f64>) -> String {
        let Some(value) = value else {
            return NONE.to_string();
        };

        if value >= 3_600.0 {
            return format!("{:.1} h", value / 3_600.0);
        }

        if value >= 60.0 {
            return format!("{:.1} min", value / 60.0);
        }

        format!("{:.1} s", value)
    }

    /// `2026-08-11T09:15:04.123+00:00` -> `09:15:04`, with the date kept when the
    /// value is not from today's clock reading (there is no local calendar here, so
    /// anything that is not parseable falls back to the raw string).
    pub fn time(value: Option<&str>) -> String {
        let Some(value) = value else {
            return NONE.to_string();
        };

        match value.split_once('T') {
            Some((_, time)) => time
                .split(['.', '+', 'Z'])
                .next()
                .unwrap_or(time)
                .to_string(),
            None => value.to_string(),
        }
    }

    /// Full timestamp, trimmed of sub-second noise — for tooltips and
    /// "last vacuum" cells where the day matters.
    pub fn date_time(value: Option<&str>) -> String {
        let Some(value) = value else {
            return NONE.to_string();
        };

        let Some((date, time)) = value.split_once('T') else {
            return value.to_string();
        };

        let time = time.split(['.', '+', 'Z']).next().unwrap_or(time);

        format!("{} {}", date, time)
    }
}

/// Wire values of a section's `state`, mirroring the server's `Section`.
///
/// Named rather than spelled out at each comparison: the three states drive what
/// the page renders, and a typo in one of four string literals would silently turn
/// "unavailable, because the extension is missing" into "no data".
pub mod section {
    pub const READY: &str = "ready";
    pub const PENDING: &str = "pending";
    pub const UNAVAILABLE: &str = "unavailable";

    pub fn is_ready(state: &str) -> bool {
        state == READY
    }
}

#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub state: String,
    pub reason: Option<String>,
    pub version: Option<String>,
    pub database_name: Option<String>,
    pub user_name: Option<String>,
    pub is_superuser: Option<bool>,
    pub can_read_all_stats: Option<bool>,
    pub has_pg_stat_statements: Option<bool>,
    pub max_connections: Option<i64>,
}

impl ServerInfo {
    /// `Postgres 16.3 · mydb · postgres` — the one-line identity of the connection.
    pub fn summary(&self) -> String {
        let version = self.version.as_deref().unwrap_or(fmt::NONE);
        let database = self.database_name.as_deref().unwrap_or(fmt::NONE);
        let user = self.user_name.as_deref().unwrap_or(fmt::NONE);

        format!("Postgres {} · {} · {}", version, database, user)
    }

    /// True only when the server said so. `None` means "not collected yet", which
    /// must not read as "this account is restricted".
    pub fn sees_all_stats(&self) -> bool {
        self.can_read_all_stats.unwrap_or(false)
    }

    pub fn is_collected(&self) -> bool {
        section::is_ready(self.state.as_str())
    }
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LongQuery {
    pub pid: Option<i64>,
    pub user_name: Option<String>,
    pub application_name: Option<String>,
    pub backend_state: Option<String>,
    pub wait: Option<String>,
    pub running_secs: Option<f64>,
    pub query: Option<String>,
}

impl LongQuery {
    /// The query text, or a statement of why it is missing — never a blank cell,
    /// which would read as "an empty query".
    pub fn query_label(&self) -> &str {
        match self.query.as_deref() {
            Some(query) if !query.trim().is_empty() => query,
            _ => "<not visible to this account>",
        }
    }

    pub fn who(&self) -> String {
        let user = self.user_name.as_deref().unwrap_or(fmt::NONE);

        match self.application_name.as_deref() {
            Some(app) if !app.trim().is_empty() => format!("{} · {}", user, app),
            _ => user.to_string(),
        }
    }
}

#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    pub state: String,
    pub reason: Option<String>,
    pub total_client_backends: Option<i64>,
    pub in_this_db: Option<i64>,
    pub active: Option<i64>,
    pub idle: Option<i64>,
    pub idle_in_transaction: Option<i64>,
    pub waiting: Option<i64>,
    pub state_unknown: Option<i64>,
    pub max_connections: Option<i64>,
    pub longest: Vec<LongQuery>,
}

impl Activity {
    /// `12 / 100` — cluster-wide client backends against `max_connections`, which
    /// is the pair that actually matters when connections run out.
    pub fn connections_label(&self) -> String {
        format!(
            "{} / {}",
            fmt::int(self.total_client_backends),
            fmt::int(self.max_connections)
        )
    }

    /// How full the connection slots are, for the tile's tone.
    pub fn connections_ratio(&self) -> Option<f64> {
        let total = self.total_client_backends? as f64;
        let max = self.max_connections? as f64;

        if max <= 0.0 {
            return None;
        }

        Some(total / max)
    }
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HealthRates {
    pub window_secs: f64,
    pub commits_per_sec: Option<f64>,
    pub rollbacks_per_sec: Option<f64>,
    pub rows_written_per_sec: Option<f64>,
    pub blks_read_per_sec: Option<f64>,
    pub cache_hit_ratio: Option<f64>,
    pub busy_backends: Option<f64>,
}

#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Health {
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
    pub rates: Option<HealthRates>,
}

impl Health {
    pub fn busy_backends(&self) -> Option<f64> {
        self.rates.as_ref()?.busy_backends
    }

    /// The window's ratio when there is one, falling back to the lifetime figure —
    /// which is labelled as such in the UI, because a lifetime 0.99 says almost
    /// nothing about right now.
    pub fn cache_hit_ratio(&self) -> Option<f64> {
        self.rates
            .as_ref()
            .and_then(|rates| rates.cache_hit_ratio)
            .or(self.lifetime_cache_hit_ratio)
    }

    pub fn cache_hit_is_windowed(&self) -> bool {
        self.rates
            .as_ref()
            .and_then(|rates| rates.cache_hit_ratio)
            .is_some()
    }

    pub fn commits_per_sec(&self) -> Option<f64> {
        self.rates.as_ref()?.commits_per_sec
    }

    pub fn rollbacks_per_sec(&self) -> Option<f64> {
        self.rates.as_ref()?.rollbacks_per_sec
    }
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Statement {
    pub query_id: Option<String>,
    pub calls: Option<i64>,
    pub total_exec_ms: Option<f64>,
    pub mean_exec_ms: Option<f64>,
    pub rows_returned: Option<i64>,
    pub blks_hit: Option<i64>,
    pub blks_read: Option<i64>,
    pub query: Option<String>,
    pub delta_calls: Option<i64>,
    pub delta_exec_ms: Option<f64>,
    pub exec_ms_per_sec: Option<f64>,
}

impl Statement {
    pub fn query_label(&self) -> &str {
        match self.query.as_deref() {
            Some(query) if !query.trim().is_empty() => query,
            _ => "<not visible to this account>",
        }
    }

    /// Milliseconds of execution per wall-clock second, as a share of one busy
    /// backend — `1000 ms/s` is one backend saturated.
    pub fn share_label(&self) -> String {
        match self.exec_ms_per_sec {
            Some(value) => format!("{:.0} ms/s", value),
            None => fmt::NONE.to_string(),
        }
    }
}

#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Load {
    pub state: String,
    pub reason: Option<String>,
    pub sees_all_statements: Option<bool>,
    pub items: Vec<Statement>,
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Table {
    pub schema: Option<String>,
    pub name: Option<String>,
    pub total_bytes: Option<i64>,
    pub table_bytes: Option<i64>,
    pub index_bytes: Option<i64>,
    pub live_tuples: Option<i64>,
    pub dead_tuples: Option<i64>,
    pub seq_scans: Option<i64>,
    pub idx_scans: Option<i64>,
    pub last_vacuum: Option<String>,
    pub last_analyze: Option<String>,
}

impl Table {
    pub fn full_name(&self) -> String {
        let schema = self.schema.as_deref().unwrap_or("?");
        let name = self.name.as_deref().unwrap_or("?");

        format!("{}.{}", schema, name)
    }

    /// Dead rows as a share of live+dead — the number that says "this table wants
    /// a vacuum". `None` when the table has no row estimates at all.
    pub fn dead_ratio(&self) -> Option<f64> {
        let live = self.live_tuples? as f64;
        let dead = self.dead_tuples? as f64;

        if live + dead <= 0.0 {
            return None;
        }

        Some(dead / (live + dead))
    }

    /// A table that has been sequentially scanned and never scanned by index is
    /// worth a second look — `idx_scans` is null in exactly that case.
    pub fn never_used_an_index(&self) -> bool {
        self.idx_scans.unwrap_or(0) == 0 && self.seq_scans.unwrap_or(0) > 0
    }
}

#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Tables {
    pub state: String,
    pub reason: Option<String>,
    pub table_count: Option<i64>,
    pub items: Vec<Table>,
}

impl Tables {
    /// `top 25 of 812` — never implies the list is everything.
    pub fn subtitle(&self) -> String {
        match self.table_count {
            Some(total) if total as usize > self.items.len() => {
                format!("top {} of {}", self.items.len(), total)
            }
            Some(total) => format!("{} tables", total),
            None => format!("{} shown", self.items.len()),
        }
    }
}

#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct DbStats {
    pub collected_at: Option<String>,
    pub slow_collected_at: Option<String>,
    pub last_error: Option<String>,
    pub server: ServerInfo,
    pub activity: Activity,
    pub health: Health,
    pub load: Load,
    pub tables: Tables,
}

#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseStats {
    pub path: String,
    pub description: String,
    pub stats: DbStats,
}

/// State of the metrics history file — server-wide.
#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct HistoryInfo {
    pub enabled: bool,
    pub path: String,
    pub retention_days: i64,
    pub open_error: Option<String>,
    pub write_error: Option<String>,
}

impl HistoryInfo {
    /// The single line that says whether anything is being recorded, and why not.
    pub fn label(&self) -> String {
        if let Some(err) = self.open_error.as_deref() {
            return format!("disabled — {}", err);
        }

        if !self.enabled {
            return "disabled".to_string();
        }

        if let Some(err) = self.write_error.as_deref() {
            return format!("recording failed — {}", err);
        }

        format!("keeping {} days", self.retention_days)
    }

    /// Enabled but failing to write is the case worth a warning: every live card
    /// keeps updating while nothing is stored.
    pub fn is_healthy(&self) -> bool {
        self.enabled && self.write_error.is_none()
    }
}

/// Mirrors `GET /api/Stats`.
///
/// `Default` is an empty list, so an unreachable server shows nothing rather than
/// inventing a database.
#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ServerStats {
    pub history: HistoryInfo,
    pub databases: Vec<DatabaseStats>,
}
