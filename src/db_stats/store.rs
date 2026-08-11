//! Three days of metrics history, in a single `redb` file.
//!
//! # Why redb and not SQLite
//!
//! The requirement was a pure-Rust store with no Rust→C marshalling. That rules
//! out every SQLite option: `rusqlite`/`libsqlite3-sys` compile the C amalgamation,
//! `async-sqlite` wraps `rusqlite`, and so does the in-house `my-sqlite` — so the
//! library this project would otherwise have reached for is disqualified by the
//! constraint rather than by its API. `turso`, the pure-Rust SQLite rewrite, is
//! still pre-1.0 and pulls `mimalloc` (C) in its default features.
//!
//! Losing SQL costs nothing here, because none of the three access patterns needs
//! it: append a sample, scan a time window, delete everything older than the
//! horizon. Those are range operations on an ordered B-tree.
//!
//! # Key order is the retention design
//!
//! Every table is keyed `(unix_micros, db_path)`, in that order, so that:
//!
//! - **retention is one bounded range sweep** — `..(cutoff, "")` covers every row
//!   older than the horizon regardless of which database wrote it. Keyed the other
//!   way round, the sweep would have to be repeated per known mount, and rows
//!   belonging to a mount since deleted from the settings file would never be
//!   collected at all — a slow leak that only shows up months later;
//! - **reads are time-bounded anyway.** A history request is always "the last N
//!   hours", so scanning a window and filtering the handful of mounts in memory
//!   costs the window, not the whole table.
//!
//! # Blocking calls
//!
//! redb is synchronous and its writes fsync. Every entry point here therefore goes
//! through `spawn_blocking`, and a tick writes **all** databases in one
//! transaction — one commit per tick rather than one per mount.

use std::sync::Arc;
use std::time::Duration;

use redb::{Database, ReadableDatabase, TableDefinition};
use rust_extensions::date_time::DateTimeAsMicroseconds;
use serde::{Deserialize, Serialize};

use super::{ActivityStats, DbHealth, Section, TablesStats, TopStatements};

/// How long history is kept. Older rows are deleted by [`MetricsStore::collect_garbage`].
pub const RETENTION: Duration = Duration::from_secs(3 * 24 * 60 * 60);

/// File name, next to the settings file in the home directory — the same place
/// the operator already looks for this server's state.
const FILE_NAME: &str = ".postgres-mcp-server-metrics.redb";

/// Overrides the history file location.
///
/// An environment variable rather than a settings key on purpose: every field in
/// [`crate::settings::SettingsModel`] is required, with no defaults anywhere, so
/// adding one there would stop every existing settings file from parsing — for a
/// path that has a perfectly good default. It also happens to be what a container
/// needs, since the default lands inside the image's `/root` and is lost on
/// restart unless a volume is pointed at it.
const PATH_ENV: &str = "POSTGRES_MCP_METRICS_PATH";

/// `(unix_micros, db_path) -> JSON`. See the module docs for why the key is in
/// this order.
const LOAD: TableDefinition<(i64, &str), &[u8]> = TableDefinition::new("load_samples");
const TABLE_SIZES: TableDefinition<(i64, &str), &[u8]> = TableDefinition::new("table_samples");
const STATEMENTS: TableDefinition<(i64, &str), &[u8]> = TableDefinition::new("statement_samples");
const LONGEST: TableDefinition<(i64, &str), &[u8]> = TableDefinition::new("longest_samples");

/// One 5-second sample of how hard the database is working.
///
/// Only rates and gauges are stored, never the raw cumulative counters: a counter
/// is meaningless without the sample before it, and after a `pg_stat_reset()` a
/// stored counter series would contain a cliff that no reader could interpret. The
/// rates are computed against the previous *in-memory* sample, where the reset is
/// still detectable — see [`super::DbHealth::new`].
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct LoadSample {
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

impl LoadSample {
    /// `None` when neither half of the tick produced anything worth storing — a
    /// row of all-nulls would pad the series with points that plot as gaps anyway.
    pub fn new(health: &Section<DbHealth>, activity: &Section<ActivityStats>) -> Option<Self> {
        let health = health.data();
        let activity = activity.data();

        if health.is_none() && activity.is_none() {
            return None;
        }

        let rates = health.and_then(|health| health.rates.as_ref());

        Some(Self {
            busy_backends: rates.and_then(|rates| rates.busy_backends),
            commits_per_sec: rates.and_then(|rates| rates.commits_per_sec),
            rollbacks_per_sec: rates.and_then(|rates| rates.rollbacks_per_sec),
            rows_written_per_sec: rates.and_then(|rates| rates.rows_written_per_sec),
            blks_read_per_sec: rates.and_then(|rates| rates.blks_read_per_sec),
            cache_hit_ratio: rates.and_then(|rates| rates.cache_hit_ratio),
            db_size_bytes: health.and_then(|health| health.db_size_bytes),
            backends_total: activity.and_then(|activity| activity.total_client_backends),
            backends_active: activity.and_then(|activity| activity.active),
            backends_idle: activity.and_then(|activity| activity.idle),
            backends_idle_in_transaction: activity
                .and_then(|activity| activity.idle_in_transaction),
            backends_waiting: activity.and_then(|activity| activity.waiting),
        })
    }
}

/// One table's size at one point in time. Narrower than
/// [`super::TableStats`] on purpose: `last_vacuum` and the scan counters are
/// useful *now* and pointless as a 3-day series, and dropping them keeps an hourly
/// row small.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TableSizeSample {
    pub schema: Option<String>,
    pub name: Option<String>,
    pub total_bytes: Option<i64>,
    pub table_bytes: Option<i64>,
    pub index_bytes: Option<i64>,
    pub live_tuples: Option<i64>,
    pub dead_tuples: Option<i64>,
}

impl TableSizeSample {
    /// The whole top-N list is stored under **one** key rather than a key per
    /// table: a reader always wants the complete picture at a moment in time, and
    /// one value per hour keeps both the write and the read to a single operation.
    fn list(tables: &Section<TablesStats>) -> Option<Vec<Self>> {
        let items = &tables.data()?.items;

        if items.is_empty() {
            return None;
        }

        Some(
            items
                .iter()
                .map(|src| Self {
                    schema: src.schema_name.clone(),
                    name: src.table_name.clone(),
                    total_bytes: src.total_bytes,
                    table_bytes: src.table_bytes,
                    index_bytes: src.index_bytes,
                    live_tuples: src.live_tuples,
                    dead_tuples: src.dead_tuples,
                })
                .collect(),
        )
    }
}

/// One statement's cost at one point in time.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StatementLoadSample {
    pub query_id: Option<String>,
    pub query: Option<String>,
    pub calls: Option<i64>,
    pub mean_exec_ms: Option<f64>,
    pub delta_exec_ms: Option<f64>,
    pub exec_ms_per_sec: Option<f64>,
}

impl StatementLoadSample {
    fn list(statements: &Section<TopStatements>) -> Option<Vec<Self>> {
        let items = &statements.data()?.items;

        if items.is_empty() {
            return None;
        }

        Some(
            items
                .iter()
                .map(|src| Self {
                    query_id: src.query_id.map(|id| id.to_string()),
                    query: src.query.clone(),
                    calls: src.calls,
                    mean_exec_ms: src.mean_exec_ms,
                    delta_exec_ms: src.delta_exec_ms,
                    exec_ms_per_sec: src.exec_ms_per_sec,
                })
                .collect(),
        )
    }
}

/// One of the hour's longest-running statements, as history.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LongestQuerySample {
    pub pid: Option<i32>,
    pub query_start: Option<String>,
    pub user_name: Option<String>,
    pub application_name: Option<String>,
    pub wait: Option<String>,
    pub running_secs: f64,
    pub query: Option<String>,
}

impl LongestQuerySample {
    /// The whole top-5 goes under one key, like the table and statement lists — a
    /// reader wants the hour's picture, not five separate rows to reassemble.
    fn list(seen: Vec<super::LongestSeen>) -> Option<Vec<Self>> {
        if seen.is_empty() {
            return None;
        }

        Some(
            seen.into_iter()
                .map(|src| Self {
                    pid: src.pid,
                    query_start: src.query_start,
                    user_name: src.user_name,
                    application_name: src.application_name,
                    wait: src.wait,
                    running_secs: src.running_secs,
                    query: src.query,
                })
                .collect(),
        )
    }
}

/// The last error a background writer hit, if any.
///
/// History writing happens on a timer with no request to fail, so an error has
/// nowhere to be returned to. Parking it here lets `GET /api/Stats` surface "the
/// numbers on this page are live but nothing is being recorded", which is
/// otherwise completely invisible — a full disk would silently stop history while
/// every card kept updating.
pub struct LastError {
    value: parking_lot::Mutex<Option<String>>,
}

impl LastError {
    pub fn new() -> Self {
        Self {
            value: parking_lot::Mutex::new(None),
        }
    }

    /// `None` clears it — a tick that succeeded means the previous failure is over.
    pub fn set(&self, error: Option<String>) {
        *self.value.lock() = error;
    }

    pub fn get(&self) -> Option<String> {
        self.value.lock().clone()
    }
}

/// What one maintenance sweep deleted.
#[derive(Debug, Clone, Copy, Default)]
pub struct GarbageCollected {
    pub load: usize,
    pub table_sizes: usize,
    pub statements: usize,
    pub longest: usize,
}

impl GarbageCollected {
    pub fn total(&self) -> usize {
        self.load + self.table_sizes + self.statements + self.longest
    }
}

/// A history row: when it was taken, and the payload.
///
/// Carries no mount path because every read is already filtered to one mount —
/// repeating it on all 2,000 rows of a window would say nothing the caller did not
/// pass in.
pub struct HistoryRow<T> {
    pub at: DateTimeAsMicroseconds,
    pub value: T,
}

/// The metrics history file.
///
/// Opening it is **not** allowed to stop the server. A read-only home directory or
/// a file left corrupt by a killed process would otherwise take down a service
/// whose actual job — proxying SQL and gating writes — does not depend on history
/// at all. So a failed open is remembered as [`Self::open_error`], every method
/// becomes a no-op that reports it, and the error travels to the UI instead of
/// being buried in a log line nobody reads.
pub struct MetricsStore {
    db: Option<Arc<Database>>,
    open_error: Option<String>,
    path: String,
}

impl MetricsStore {
    /// Opens (or creates) the history file — [`PATH_ENV`] when set, otherwise
    /// [`FILE_NAME`] in the home directory.
    pub async fn open() -> Self {
        let configured = std::env::var(PATH_ENV)
            .ok()
            .map(|path| path.trim().to_string())
            .filter(|path| !path.is_empty())
            .unwrap_or_else(|| format!("~/{}", FILE_NAME));

        // `~` is expanded either way, so the override can be written the same way
        // the settings file path is.
        let path = rust_extensions::file_utils::format_path(configured).to_string();

        Self::open_at(path).await
    }

    /// Opens a history file at an explicit path. Exists so the tests can exercise
    /// the real store — the retention sweep and the tuple-key ranges are the two
    /// things here that a type checker cannot verify.
    pub async fn open_at(path: String) -> Self {
        let opening = path.clone();

        // `Database::create` opens an existing file and creates a missing one, and
        // it is blocking either way.
        let opened = tokio::task::spawn_blocking(move || open_and_init(opening.as_str()))
            .await
            .unwrap_or_else(|err| Err(format!("The history file open task panicked: {:?}", err)));

        match opened {
            Ok(db) => Self {
                db: Some(Arc::new(db)),
                open_error: None,
                path,
            },
            Err(err) => {
                println!(
                    "Metrics history is disabled: could not open '{}'. {}",
                    path, err
                );

                Self {
                    db: None,
                    open_error: Some(err),
                    path,
                }
            }
        }
    }

    pub fn path(&self) -> &str {
        self.path.as_str()
    }

    /// `Some(reason)` when history is unavailable for this run.
    pub fn open_error(&self) -> Option<&str> {
        self.open_error.as_deref()
    }

    pub fn is_enabled(&self) -> bool {
        self.db.is_some()
    }

    /// One commit for every database's 5-second sample.
    pub async fn write_load(
        &self,
        at: DateTimeAsMicroseconds,
        samples: Vec<(String, LoadSample)>,
    ) -> Result<(), String> {
        let rows = encode_rows(samples)?;

        self.write(LOAD, at, rows).await
    }

    /// The hourly rows. All three are written in one transaction, so an interrupted
    /// sweep cannot leave an hour with table sizes but no statements beside them.
    pub async fn write_hourly(
        &self,
        at: DateTimeAsMicroseconds,
        tables: &Section<TablesStats>,
        statements: &Section<TopStatements>,
        longest: Vec<super::LongestSeen>,
        db_path: &str,
    ) -> Result<(), String> {
        let Some(db) = self.db.clone() else {
            return Err(self.disabled());
        };

        let table_sizes = TableSizeSample::list(tables)
            .map(|list| serde_json::to_vec(&list))
            .transpose()
            .map_err(|err| format!("Could not encode the table sizes: {}", err))?;

        let statements = StatementLoadSample::list(statements)
            .map(|list| serde_json::to_vec(&list))
            .transpose()
            .map_err(|err| format!("Could not encode the statements: {}", err))?;

        let longest = LongestQuerySample::list(longest)
            .map(|list| serde_json::to_vec(&list))
            .transpose()
            .map_err(|err| format!("Could not encode the longest queries: {}", err))?;

        if table_sizes.is_none() && statements.is_none() && longest.is_none() {
            return Ok(());
        }

        let db_path = db_path.to_string();
        let at = at.unix_microseconds;

        spawn_blocking(move || {
            let write = db.begin_write().map_err(to_string)?;

            {
                for (definition, payload) in [
                    (TABLE_SIZES, table_sizes),
                    (STATEMENTS, statements),
                    (LONGEST, longest),
                ] {
                    let Some(payload) = payload else {
                        continue;
                    };

                    let mut table = write.open_table(definition).map_err(to_string)?;
                    table
                        .insert((at, db_path.as_str()), payload.as_slice())
                        .map_err(to_string)?;
                }
            }

            write.commit().map_err(to_string)
        })
        .await
    }

    pub async fn read_load(
        &self,
        from: DateTimeAsMicroseconds,
        to: DateTimeAsMicroseconds,
        db_path: &str,
    ) -> Result<Vec<HistoryRow<LoadSample>>, String> {
        self.read(LOAD, from, to, db_path).await
    }

    pub async fn read_table_sizes(
        &self,
        from: DateTimeAsMicroseconds,
        to: DateTimeAsMicroseconds,
        db_path: &str,
    ) -> Result<Vec<HistoryRow<Vec<TableSizeSample>>>, String> {
        self.read(TABLE_SIZES, from, to, db_path).await
    }

    pub async fn read_statements(
        &self,
        from: DateTimeAsMicroseconds,
        to: DateTimeAsMicroseconds,
        db_path: &str,
    ) -> Result<Vec<HistoryRow<Vec<StatementLoadSample>>>, String> {
        self.read(STATEMENTS, from, to, db_path).await
    }

    /// Each row is one hour's top-5 longest-running statements, longest first.
    pub async fn read_longest(
        &self,
        from: DateTimeAsMicroseconds,
        to: DateTimeAsMicroseconds,
        db_path: &str,
    ) -> Result<Vec<HistoryRow<Vec<LongestQuerySample>>>, String> {
        self.read(LONGEST, from, to, db_path).await
    }

    /// Deletes everything older than [`RETENTION`], across every database and
    /// every table, in one transaction.
    pub async fn collect_garbage(&self) -> Result<GarbageCollected, String> {
        self.collect_garbage_before(DateTimeAsMicroseconds::now().sub(RETENTION))
            .await
    }

    /// The horizon as a parameter, so the tests can age rows without waiting three
    /// days.
    pub async fn collect_garbage_before(
        &self,
        cutoff: DateTimeAsMicroseconds,
    ) -> Result<GarbageCollected, String> {
        let Some(db) = self.db.clone() else {
            return Err(self.disabled());
        };

        let cutoff = cutoff.unix_microseconds;

        spawn_blocking(move || {
            let write = db.begin_write().map_err(to_string)?;

            let collected = {
                GarbageCollected {
                    load: delete_older_than(&write, LOAD, cutoff)?,
                    table_sizes: delete_older_than(&write, TABLE_SIZES, cutoff)?,
                    statements: delete_older_than(&write, STATEMENTS, cutoff)?,
                    longest: delete_older_than(&write, LONGEST, cutoff)?,
                }
            };

            write.commit().map_err(to_string)?;

            Ok(collected)
        })
        .await
    }

    async fn write(
        &self,
        definition: TableDefinition<'static, (i64, &'static str), &'static [u8]>,
        at: DateTimeAsMicroseconds,
        rows: Vec<(String, Vec<u8>)>,
    ) -> Result<(), String> {
        let Some(db) = self.db.clone() else {
            return Err(self.disabled());
        };

        if rows.is_empty() {
            return Ok(());
        }

        let at = at.unix_microseconds;

        spawn_blocking(move || {
            let write = db.begin_write().map_err(to_string)?;

            {
                let mut table = write.open_table(definition).map_err(to_string)?;

                for (db_path, payload) in &rows {
                    table
                        .insert((at, db_path.as_str()), payload.as_slice())
                        .map_err(to_string)?;
                }
            }

            write.commit().map_err(to_string)
        })
        .await
    }

    async fn read<T: for<'de> Deserialize<'de> + Send + 'static>(
        &self,
        definition: TableDefinition<'static, (i64, &'static str), &'static [u8]>,
        from: DateTimeAsMicroseconds,
        to: DateTimeAsMicroseconds,
        db_path: &str,
    ) -> Result<Vec<HistoryRow<T>>, String> {
        let Some(db) = self.db.clone() else {
            return Err(self.disabled());
        };

        let db_path = db_path.to_string();
        let (from, to) = (from.unix_microseconds, to.unix_microseconds);

        spawn_blocking(move || {
            let read = db.begin_read().map_err(to_string)?;
            let table = read.open_table(definition).map_err(to_string)?;

            let mut result = Vec::new();

            // The window bounds the scan; the mount filter runs in memory because a
            // handful of mounts share each timestamp. See the module docs.
            for row in table.range((from, "")..=(to, MAX_PATH)).map_err(to_string)? {
                let (key, value) = row.map_err(to_string)?;
                let (at, path) = key.value();

                if !crate::settings::paths_are_equal(path, db_path.as_str()) {
                    continue;
                }

                let value: T = serde_json::from_slice(value.value())
                    .map_err(|err| format!("Could not decode a history row: {}", err))?;

                result.push(HistoryRow {
                    at: DateTimeAsMicroseconds::new(at),
                    value,
                });
            }

            Ok(result)
        })
        .await
    }

    fn disabled(&self) -> String {
        self.open_error
            .clone()
            .unwrap_or_else(|| "Metrics history is disabled.".to_string())
    }
}

/// Upper bound for the `db_path` half of a key, so a time range can be closed at
/// `to` without knowing which mounts exist. `\u{10FFFF}` sorts above every path a
/// settings file can produce, since paths are compared as UTF-8 bytes.
const MAX_PATH: &str = "\u{10FFFF}";

/// Creating the tables up front matters: `open_table` inside a **read**
/// transaction fails on a table that has never been written, so a history request
/// made before the first tick would report an error rather than an empty series.
fn open_and_init(path: &str) -> Result<Database, String> {
    let db = Database::create(path).map_err(to_string)?;

    let write = db.begin_write().map_err(to_string)?;

    {
        write.open_table(LOAD).map_err(to_string)?;
        write.open_table(TABLE_SIZES).map_err(to_string)?;
        write.open_table(STATEMENTS).map_err(to_string)?;
        write.open_table(LONGEST).map_err(to_string)?;
    }

    write.commit().map_err(to_string)?;

    Ok(db)
}

/// Bounded sweep: only the rows below the horizon are visited, so the cost is
/// proportional to what is being deleted rather than to the size of the history.
///
/// `extract_from_if` removes an entry only once it has been *read* from the
/// iterator, so the loop has to drain it — a `.count()` that short-circuited would
/// silently leave rows behind.
fn delete_older_than(
    write: &redb::WriteTransaction,
    definition: TableDefinition<'static, (i64, &'static str), &'static [u8]>,
    cutoff: i64,
) -> Result<usize, String> {
    let mut table = write.open_table(definition).map_err(to_string)?;

    let mut removed = 0;

    let mut expired = table
        .extract_from_if((i64::MIN, "")..(cutoff, ""), |_, _| true)
        .map_err(to_string)?;

    while let Some(row) = expired.next() {
        row.map_err(to_string)?;
        removed += 1;
    }

    Ok(removed)
}

fn encode_rows<T: Serialize>(rows: Vec<(String, T)>) -> Result<Vec<(String, Vec<u8>)>, String> {
    rows.into_iter()
        .map(|(db_path, value)| {
            serde_json::to_vec(&value)
                .map(|payload| (db_path, payload))
                .map_err(|err| format!("Could not encode a history row: {}", err))
        })
        .collect()
}

/// redb is synchronous and its commits fsync, so every call has to leave the async
/// worker threads.
async fn spawn_blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|err| Err(format!("The history task panicked: {:?}", err)))
}

fn to_string(err: impl std::fmt::Debug) -> String {
    format!("{:?}", err)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh file per test, in the OS temp directory. redb creates it on open, so
    /// only the name has to be unique.
    fn temp_file(name: &str) -> String {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "postgres-mcp-server-test-{}-{}.redb",
            name,
            std::process::id()
        ));

        let path = path.to_string_lossy().to_string();
        let _ = std::fs::remove_file(path.as_str());
        path
    }

    fn at(secs: i64) -> DateTimeAsMicroseconds {
        DateTimeAsMicroseconds::new(secs * 1_000_000)
    }

    fn load_sample(busy: f64) -> LoadSample {
        LoadSample {
            busy_backends: Some(busy),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_window_read_returns_only_the_requested_mount_oldest_first() {
        let path = temp_file("window");
        let store = MetricsStore::open_at(path.clone()).await;
        assert!(store.is_enabled(), "{:?}", store.open_error());

        // Two mounts sampled at the same three instants.
        for (secs, busy) in [(1_000i64, 1.0f64), (1_010, 2.0), (1_020, 3.0)] {
            store
                .write_load(
                    at(secs),
                    vec![
                        ("/mcp".to_string(), load_sample(busy)),
                        ("/other".to_string(), load_sample(busy + 100.0)),
                    ],
                )
                .await
                .unwrap();
        }

        let rows = store.read_load(at(1_000), at(1_020), "/mcp").await.unwrap();

        assert_eq!(rows.len(), 3);
        // Oldest first, so a chart can plot straight through.
        assert_eq!(rows[0].value.busy_backends, Some(1.0));
        assert_eq!(rows[2].value.busy_backends, Some(3.0));

        // The other mount's rows share the same timestamps and must not leak in.
        let other = store
            .read_load(at(1_000), at(1_020), "/other")
            .await
            .unwrap();
        assert_eq!(other[0].value.busy_backends, Some(101.0));

        // The window is inclusive of `to` and excludes what sits outside it.
        let narrow = store.read_load(at(1_005), at(1_015), "/mcp").await.unwrap();
        assert_eq!(narrow.len(), 1);
        assert_eq!(narrow[0].value.busy_backends, Some(2.0));

        let _ = std::fs::remove_file(path.as_str());
    }

    #[tokio::test]
    async fn the_mount_filter_matches_the_way_requests_are_routed() {
        let path = temp_file("case");
        let store = MetricsStore::open_at(path.clone()).await;

        store
            .write_load(at(2_000), vec![("/mcp".to_string(), load_sample(1.0))])
            .await
            .unwrap();

        // Paths are compared case-insensitively everywhere else in this server.
        let rows = store.read_load(at(1_999), at(2_001), "/MCP").await.unwrap();
        assert_eq!(rows.len(), 1);

        let _ = std::fs::remove_file(path.as_str());
    }

    #[tokio::test]
    async fn the_sweep_deletes_every_mount_past_the_horizon_and_nothing_after_it() {
        let path = temp_file("gc");
        let store = MetricsStore::open_at(path.clone()).await;

        for secs in [1_000i64, 2_000, 3_000, 4_000] {
            store
                .write_load(
                    at(secs),
                    vec![
                        ("/mcp".to_string(), load_sample(1.0)),
                        // A mount that will be "removed from the settings file":
                        // its rows still have to be collected, which is the whole
                        // reason the key leads with the timestamp.
                        ("/gone".to_string(), load_sample(2.0)),
                    ],
                )
                .await
                .unwrap();
        }

        let collected = store.collect_garbage_before(at(3_000)).await.unwrap();

        // 1_000 and 2_000 for both mounts; 3_000 is the cutoff and survives.
        assert_eq!(collected.load, 4);
        assert_eq!(collected.total(), 4);

        let kept = store.read_load(at(0), at(9_999), "/mcp").await.unwrap();
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].at.unix_microseconds, at(3_000).unix_microseconds);

        let orphaned = store.read_load(at(0), at(9_999), "/gone").await.unwrap();
        assert_eq!(orphaned.len(), 2);

        // A second sweep at the same horizon has nothing left to do.
        assert_eq!(store.collect_garbage_before(at(3_000)).await.unwrap().load, 0);

        let _ = std::fs::remove_file(path.as_str());
    }

    #[tokio::test]
    async fn a_history_read_before_the_first_write_is_an_empty_series_not_an_error() {
        // `open_table` in a read transaction fails on a table that was never
        // written, which is why the tables are created at open time.
        let path = temp_file("empty");
        let store = MetricsStore::open_at(path.clone()).await;

        assert!(store.read_load(at(0), at(9_999), "/mcp").await.unwrap().is_empty());
        assert!(
            store
                .read_table_sizes(at(0), at(9_999), "/mcp")
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .read_statements(at(0), at(9_999), "/mcp")
                .await
                .unwrap()
                .is_empty()
        );

        let _ = std::fs::remove_file(path.as_str());
    }

    #[tokio::test]
    async fn a_disabled_store_reports_its_reason_instead_of_panicking() {
        // A directory is not a database file, so the open fails the way a
        // read-only home directory or a corrupt file would.
        let store = MetricsStore::open_at(std::env::temp_dir().to_string_lossy().to_string()).await;

        assert!(!store.is_enabled());
        assert!(store.open_error().is_some());
        assert!(store.write_load(at(1), Vec::new()).await.is_err());
        assert!(store.read_load(at(0), at(9), "/mcp").await.is_err());
        assert!(store.collect_garbage().await.is_err());
    }

    #[tokio::test]
    async fn an_hourly_row_carries_both_lists_and_survives_a_round_trip() {
        let path = temp_file("hourly");
        let store = MetricsStore::open_at(path.clone()).await;

        let tables = Section::Ready(TablesStats {
            table_count: Some(2),
            items: vec![crate::db_stats::TableStats {
                schema_name: Some("public".to_string()),
                table_name: Some("users".to_string()),
                total_bytes: Some(4096),
                table_bytes: Some(2048),
                index_bytes: Some(2048),
                live_tuples: Some(10),
                dead_tuples: Some(1),
                seq_scans: Some(3),
                idx_scans: None,
                last_vacuum: None,
                last_analyze: None,
            }],
        });

        let statements = Section::Ready(TopStatements {
            sees_all_statements: true,
            items: vec![crate::db_stats::TopStatement {
                query_id: Some(42),
                calls: Some(7),
                total_exec_ms: Some(500.0),
                mean_exec_ms: Some(71.4),
                rows_returned: Some(7),
                blks_hit: Some(1),
                blks_read: Some(0),
                query: Some("SELECT $1".to_string()),
                delta_calls: Some(2),
                delta_exec_ms: Some(100.0),
                exec_ms_per_sec: Some(1.7),
            }],
        });

        let longest = vec![super::super::LongestSeen {
            pid: Some(99),
            query_start: Some("2026-08-11T10:00:00+00:00".to_string()),
            user_name: Some("reader".to_string()),
            application_name: None,
            wait: Some("Lock: transactionid".to_string()),
            running_secs: 312.5,
            query: Some("UPDATE accumulator_values SET x = $1".to_string()),
        }];

        store
            .write_hourly(at(5_000), &tables, &statements, longest, "/mcp")
            .await
            .unwrap();

        let sizes = store
            .read_table_sizes(at(4_999), at(5_001), "/mcp")
            .await
            .unwrap();
        assert_eq!(sizes.len(), 1);
        assert_eq!(sizes[0].value[0].name.as_deref(), Some("users"));
        assert_eq!(sizes[0].value[0].total_bytes, Some(4096));

        let costs = store
            .read_statements(at(4_999), at(5_001), "/mcp")
            .await
            .unwrap();
        assert_eq!(costs.len(), 1);
        // queryid crosses the wire as a string so a 64-bit hash survives a JSON
        // reader.
        assert_eq!(costs[0].value[0].query_id.as_deref(), Some("42"));
        assert_eq!(costs[0].value[0].exec_ms_per_sec, Some(1.7));

        // All three hourly lists share one timestamp, written in one transaction.
        let slowest = store
            .read_longest(at(4_999), at(5_001), "/mcp")
            .await
            .unwrap();
        assert_eq!(slowest.len(), 1);
        assert_eq!(slowest[0].value[0].running_secs, 312.5);
        assert_eq!(
            slowest[0].value[0].wait.as_deref(),
            Some("Lock: transactionid")
        );

        let _ = std::fs::remove_file(path.as_str());
    }

    #[tokio::test]
    async fn nothing_is_written_when_a_tick_produced_no_sections() {
        let path = temp_file("nothing");
        let store = MetricsStore::open_at(path.clone()).await;

        // Both sections pending: there is nothing to record, and a row of nulls
        // would plot as "zero load" rather than as the gap it really is.
        assert!(LoadSample::new(&Section::Pending, &Section::Pending).is_none());

        store
            .write_hourly(
                at(6_000),
                &Section::Pending,
                &Section::Pending,
                Vec::new(),
                "/mcp",
            )
            .await
            .unwrap();

        assert!(
            store
                .read_table_sizes(at(0), at(9_999), "/mcp")
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .read_longest(at(0), at(9_999), "/mcp")
                .await
                .unwrap()
                .is_empty()
        );

        let _ = std::fs::remove_file(path.as_str());
    }
}
