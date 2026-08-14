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
    /// Off by default. Without it every I/O *timing* figure is `None` rather than
    /// zero, because Postgres reports a hard zero when nothing is being measured.
    pub track_io_timing: Option<bool>,
    /// The contrib package is installed, so the library is on disk. Gates the offer
    /// to touch `shared_preload_libraries`: naming a library that is not there stops
    /// Postgres from starting.
    pub pg_stat_statements_available: Option<bool>,
    /// Already preloaded — `CREATE EXTENSION` alone finishes the job, no restart.
    pub pg_stat_statements_preloaded: Option<bool>,
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

/// One client backend of this database, whatever it is doing.
#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub pid: Option<i64>,
    pub user_name: Option<String>,
    pub application_name: Option<String>,
    /// `None` over the unix socket — which is what Postgres running next to its
    /// client looks like, not a value the server withheld.
    pub client_addr: Option<String>,
    pub backend_state: Option<String>,
    pub wait: Option<String>,
    pub connected_at: Option<String>,
    pub connected_secs: Option<f64>,
    /// Time in the current state. On `idle in transaction` this is the age of a
    /// transaction that is still holding its locks.
    pub state_secs: Option<f64>,
    pub running_secs: Option<f64>,
    /// The current statement — or, on an idle backend, the one it ran last.
    pub query: Option<String>,
    /// This server's own collector connection. It holds a real connection slot, so
    /// it is listed like any other and labelled instead of being dropped.
    pub is_collector: bool,
}

impl Connection {
    pub fn who(&self) -> String {
        let user = self.user_name.as_deref().unwrap_or(fmt::NONE);

        match self.application_name.as_deref() {
            Some(app) if !app.trim().is_empty() => format!("{} · {}", user, app),
            _ => user.to_string(),
        }
    }

    /// A backend with no `client_addr` came in over the unix socket. Saying so beats
    /// a `—`, which would read as "the server does not know where this is from".
    pub fn client_label(&self) -> &str {
        match self.client_addr.as_deref() {
            Some(addr) if !addr.trim().is_empty() => addr,
            _ => "local",
        }
    }

    /// The backend's state, or a statement that this account may not read it —
    /// never a blank cell, which would read as a backend doing nothing.
    pub fn state_label(&self) -> &str {
        match self.backend_state.as_deref() {
            Some(state) if !state.trim().is_empty() => state,
            _ => "not visible",
        }
    }

    pub fn is_active(&self) -> bool {
        self.backend_state.as_deref() == Some("active")
    }

    /// Includes `idle in transaction (aborted)`, which is the same problem with a
    /// failed statement in front of it.
    pub fn is_idle_in_transaction(&self) -> bool {
        self.backend_state
            .as_deref()
            .map(|state| state.starts_with("idle in transaction"))
            .unwrap_or(false)
    }

    /// The query text, or why there is none. On a backend that is not active this is
    /// the last statement it ran rather than a running one — the state column next to
    /// it is what says which.
    ///
    /// Postgres has two different ways of saying nothing here, and they are not the
    /// same thing: an **empty** string on a backend whose state is readable means it
    /// has genuinely not run a statement yet (a connection a pool has just opened
    /// looks exactly like this), while a backend this account may not inspect has its
    /// query text blanked along with its state. Reporting the first as the second
    /// would blame the account for a connection that is simply new.
    pub fn query_label(&self) -> &str {
        match self.query.as_deref() {
            Some(query) if !query.trim().is_empty() => query,
            _ if self.backend_state.is_none() => "<not visible to this account>",
            _ => fmt::NONE,
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
    /// Every client backend on this database, busiest first. Capped by the server at
    /// 100 rows, which is what [`Activity::connections_subtitle`] compares against
    /// `in_this_db` to detect.
    pub connections: Vec<Connection>,
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

    /// What the connections card says it is showing.
    ///
    /// The server caps the list, so this says `100 of 214` whenever it was cut: a
    /// table quietly showing a hundred of two hundred connections would be read as
    /// the whole truth.
    ///
    /// The count and the total come from two queries a few milliseconds apart, so a
    /// connection that opened in between can make the list the longer of the two.
    /// That is not a cut list, and is reported as the plain count rather than as
    /// `41 of 40`.
    pub fn connections_subtitle(&self) -> String {
        let shown = self.connections.len() as i64;

        match self.in_this_db {
            Some(total) if total > shown => format!("{} of {}", shown, fmt::group(total)),
            _ => format!("{} connected", fmt::group(shown)),
        }
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
    pub io_read_ms_per_sec: Option<f64>,
    pub io_write_ms_per_sec: Option<f64>,
    pub io_share_of_active: Option<f64>,
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
    pub blk_read_time_ms: Option<f64>,
    pub blk_write_time_ms: Option<f64>,
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

    /// Milliseconds per second lost to reads and writes together — the headline
    /// damage figure. `None` when `track_io_timing` is off, which the tile renders
    /// as `—` rather than as a reassuring zero.
    pub fn io_wait_ms_per_sec(&self) -> Option<f64> {
        let rates = self.rates.as_ref()?;

        match (rates.io_read_ms_per_sec, rates.io_write_ms_per_sec) {
            (Some(read), Some(write)) => Some(read + write),
            (Some(read), None) => Some(read),
            (None, Some(write)) => Some(write),
            (None, None) => None,
        }
    }

    pub fn io_share_of_active(&self) -> Option<f64> {
        self.rates.as_ref()?.io_share_of_active
    }
}

/// One table's read cost, from `pg_statio_user_tables`.
#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TableIo {
    pub schema: Option<String>,
    pub name: Option<String>,
    pub total_read_blocks: Option<i64>,
    pub heap_read_blocks: Option<i64>,
    pub index_read_blocks: Option<i64>,
    pub toast_read_blocks: Option<i64>,
    pub cache_hit_ratio: Option<f64>,
    pub delta_read_blocks: Option<i64>,
    pub delta_read_bytes: Option<i64>,
    pub read_bytes_per_sec: Option<f64>,
}

impl TableIo {
    pub fn full_name(&self) -> String {
        format!(
            "{}.{}",
            self.schema.as_deref().unwrap_or("?"),
            self.name.as_deref().unwrap_or("?")
        )
    }
}

/// Cluster-wide write accounting.
#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WriteIo {
    pub wal_bytes: Option<i64>,
    pub wal_records: Option<i64>,
    pub wal_full_page_images: Option<i64>,
    pub wal_bytes_per_sec: Option<f64>,
    pub checkpoints_timed: Option<i64>,
    pub checkpoints_requested: Option<i64>,
    pub buffers_written_by_checkpointer: Option<i64>,
    pub buffers_written_by_bgwriter: Option<i64>,
    pub buffers_written_by_backends: Option<i64>,
}

impl WriteIo {
    /// Share of checkpoints forced early by `max_wal_size` rather than run on the
    /// timer. Above roughly a third is the classic sign the setting is too small.
    pub fn forced_checkpoint_ratio(&self) -> Option<f64> {
        let timed = self.checkpoints_timed? as f64;
        let requested = self.checkpoints_requested? as f64;

        if timed + requested <= 0.0 {
            return None;
        }

        Some(requested / (timed + requested))
    }
}

#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct DiskIo {
    pub state: String,
    pub reason: Option<String>,
    pub io_timing_enabled: Option<bool>,
    pub tables: Vec<TableIo>,
    pub writes: Option<WriteIo>,
    /// Why the write half is missing, when it is — the read half can still work.
    pub writes_unavailable: Option<String>,
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
    pub disk_io: DiskIo,
    /// The last completed minute. `None` until two slow ticks have run — a minute
    /// needs two readings to exist at all.
    pub throughput: Option<Throughput>,
}

/// One minute of traffic across the whole database.
#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Throughput {
    pub window_secs: f64,
    /// Statements that completed in the window — every one the database ran, not
    /// just the agent's. Exact, from `pg_stat_statements`.
    pub calls: Option<i64>,
    pub calls_per_sec: Option<f64>,
    pub avg_exec_ms: Option<f64>,
    pub total_exec_ms: Option<f64>,
    /// Longest execution the 5-second sampler *saw*. A floor: anything that starts
    /// and finishes between two samples is invisible.
    pub longest_secs: Option<f64>,
    pub longest_query: Option<String>,
    /// A new lifetime maximum set inside the window — exact when present.
    pub slowest_finished_ms: Option<f64>,
    pub slowest_finished_query: Option<String>,
}

impl Throughput {
    /// A tick that ran late makes "per minute" misleading; the card then leads with
    /// the rate instead of the raw count.
    pub fn window_is_about_a_minute(&self) -> bool {
        (45.0..=90.0).contains(&self.window_secs)
    }

    pub fn window_label(&self) -> String {
        if self.window_is_about_a_minute() {
            "last minute".to_string()
        } else {
            format!("last {:.0}s", self.window_secs)
        }
    }
}

/// Which Postgres server a database lives on, derived by the server from the
/// connection string. Carries no credentials.
#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ServerRef {
    /// Databases sharing this key are on one server. The page groups and switches
    /// by it.
    pub key: String,
    pub label: String,
    pub host: String,
    pub port: i64,
    /// The database this mount opens, from the connection string.
    pub database: Option<String>,
    /// Server + database. Two mounts sharing this open the **same** database and
    /// therefore read identical counters — the page marks the second one as a copy
    /// rather than drawing it as extra load. `None` when the connection string does
    /// not name a database, in which case sameness cannot be proven and the mounts
    /// are treated as distinct.
    pub database_key: Option<String>,
}

#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseStats {
    pub path: String,
    pub description: String,
    pub server: ServerRef,
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

impl ServerStats {
    /// The distinct Postgres servers behind the configured databases, in the order
    /// the settings file declares them.
    ///
    /// Several mounts routinely share one server, and with a single server there is
    /// nothing to switch between — which is why the page asks for this rather than
    /// assuming one section per database.
    pub fn servers(&self) -> Vec<ServerRef> {
        let mut result: Vec<ServerRef> = Vec::new();

        for db in &self.databases {
            if !result.iter().any(|known| known.key == db.server.key) {
                result.push(db.server.clone());
            }
        }

        result
    }

    /// The databases on one server, in declaration order.
    pub fn databases_on(&self, server_key: &str) -> Vec<DatabaseStats> {
        self.databases
            .iter()
            .filter(|db| db.server.key == server_key)
            .cloned()
            .collect()
    }
}

/// Body of `POST /api/Settings/PgStatStatements`.
#[derive(serde::Serialize)]
pub struct SetupExtensionRequest {
    pub path: String,
    /// `"create"` or `"preload"`.
    pub action: String,
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SetupExtensionResult {
    pub done: bool,
    /// Postgres must be restarted before the change takes effect — a reload is not
    /// enough for `shared_preload_libraries`.
    pub restart_required: bool,
    pub message: String,
    pub error: Option<String>,
}

/// Body of `POST /api/Settings/TrackIoTiming`.
#[derive(serde::Serialize)]
pub struct SetTrackIoTimingRequest {
    pub path: String,
    pub enabled: bool,
}

/// What the server reports after the attempt.
///
/// `enabled` is what the *server* says afterwards, not what was asked for —
/// `ALTER SYSTEM` only edits a file, so a request that did not error is still not
/// proof the setting is on.
#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TrackIoTimingResult {
    pub ok: bool,
    pub enabled: Option<bool>,
    /// Postgres' own message. "must be superuser" and "cannot run inside a
    /// transaction block" need different fixes, so it is shown verbatim.
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn throughput(window_secs: f64) -> Throughput {
        Throughput {
            window_secs,
            calls: None,
            calls_per_sec: None,
            avg_exec_ms: None,
            total_exec_ms: None,
            longest_secs: None,
            longest_query: None,
            slowest_finished_ms: None,
            slowest_finished_query: None,
        }
    }

    #[test]
    fn a_tick_that_ran_late_is_not_labelled_as_a_minute() {
        // Calling a 4-minute window "last minute" would misstate every figure in the
        // card by a factor of four.
        assert_eq!(throughput(60.0).window_label(), "last minute");
        assert_eq!(throughput(62.5).window_label(), "last minute");
        assert_eq!(throughput(240.0).window_label(), "last 240s");
    }

    #[test]
    fn bytes_are_rendered_the_way_postgres_renders_them() {
        assert_eq!(fmt::bytes(Some(512)), "512 B");
        assert_eq!(fmt::bytes(Some(2 * 1024 * 1024)), "2.0 MB");
        assert_eq!(fmt::bytes(Some(2_147_483_648)), "2.0 GB");
        // A value the database could not report is never a zero.
        assert_eq!(fmt::bytes(None), fmt::NONE);
    }

    fn connection(state: Option<&str>) -> Connection {
        Connection {
            pid: Some(42),
            user_name: Some("postgres".to_string()),
            application_name: None,
            client_addr: None,
            backend_state: state.map(|state| state.to_string()),
            wait: None,
            connected_at: None,
            connected_secs: Some(10.0),
            state_secs: Some(1.0),
            running_secs: None,
            query: None,
            is_collector: false,
        }
    }

    fn activity(connections: Vec<Connection>, in_this_db: Option<i64>) -> Activity {
        Activity {
            in_this_db,
            connections,
            ..Default::default()
        }
    }

    #[test]
    fn a_cut_connection_list_says_so() {
        // The server caps the list; the card has to admit it rather than showing a
        // hundred rows as if they were all of them.
        let capped = activity(vec![connection(Some("idle")); 100], Some(214));
        assert_eq!(capped.connections_subtitle(), "100 of 214");

        let complete = activity(vec![connection(Some("idle")); 3], Some(3));
        assert_eq!(complete.connections_subtitle(), "3 connected");
    }

    #[test]
    fn a_connection_opened_between_the_two_queries_is_not_a_cut_list() {
        // The count and the total are read a few milliseconds apart, so the list can
        // be the longer of the two. "4 of 3" would be nonsense.
        let raced = activity(vec![connection(Some("idle")); 4], Some(3));

        assert_eq!(raced.connections_subtitle(), "4 connected");
    }

    #[test]
    fn an_aborted_transaction_is_still_idle_in_transaction() {
        // Postgres reports it as "idle in transaction (aborted)" — the same held
        // locks, with a failed statement in front of them.
        assert!(connection(Some("idle in transaction")).is_idle_in_transaction());
        assert!(connection(Some("idle in transaction (aborted)")).is_idle_in_transaction());

        assert!(!connection(Some("idle")).is_idle_in_transaction());
        assert!(!connection(Some("active")).is_idle_in_transaction());
        assert!(!connection(None).is_idle_in_transaction());
    }

    #[test]
    fn a_backend_with_no_client_address_came_in_over_the_unix_socket() {
        assert_eq!(connection(None).client_label(), "local");

        let mut remote = connection(Some("active"));
        remote.client_addr = Some("10.0.0.4".to_string());
        assert_eq!(remote.client_label(), "10.0.0.4");
    }

    #[test]
    fn a_connection_that_has_run_nothing_is_not_reported_as_a_privilege_problem() {
        // A pool that has just opened a connection reports an empty query with a
        // perfectly readable state — seen on a live server. Only a blanked *state*
        // means the account cannot look.
        let mut fresh = connection(Some("idle"));
        fresh.query = Some(String::new());
        assert_eq!(fresh.query_label(), fmt::NONE);

        let hidden = connection(None);
        assert_eq!(hidden.query_label(), "<not visible to this account>");

        let mut running = connection(Some("active"));
        running.query = Some("SELECT 1".to_string());
        assert_eq!(running.query_label(), "SELECT 1");
    }

    #[test]
    fn a_state_this_account_may_not_read_is_never_a_blank_cell() {
        // A blank would read as a backend doing nothing, which is the one thing it
        // does not mean.
        assert_eq!(connection(None).state_label(), "not visible");
        assert_eq!(connection(Some("active")).state_label(), "active");
    }

    #[test]
    fn a_missing_number_is_never_shown_as_zero() {
        assert_eq!(fmt::int(None), fmt::NONE);
        assert_eq!(fmt::ratio(None), fmt::NONE);
        assert_eq!(fmt::millis(None), fmt::NONE);
        assert_eq!(fmt::seconds(None), fmt::NONE);
    }
}
