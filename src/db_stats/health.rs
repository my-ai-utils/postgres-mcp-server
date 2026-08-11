use std::time::Duration;

use my_postgres::tokio_postgres::Row;
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::postgres::{PostgresAccess, opt_f64, opt_i64, opt_timestamp, stats_row};

use super::ServerCapabilities;

/// `pg_stat_database.active_time` / `.session_time` were added in 14. Below that
/// the busy-backends figure — the whole reason this section exists — cannot be
/// computed, and the columns are selected as NULL rather than making the query
/// fail outright, so the counters that *do* exist still come through.
const PG14: i32 = 140000;

/// One raw read of `pg_stat_database`.
///
/// Kept separately from [`DbHealth`] because every number in this view is
/// **cumulative since the last stats reset**. On its own that answers "how many
/// commits has this database ever done", which nobody is asking. The useful
/// figure is the difference between two reads, so the raw sample is retained and
/// the rates are derived — see [`DbHealth::new`].
#[derive(Debug, Clone)]
pub struct DbHealthSample {
    pub taken_at: DateTimeAsMicroseconds,
    pub db_size_bytes: Option<i64>,
    pub num_backends: Option<i64>,
    pub commits: Option<i64>,
    pub rollbacks: Option<i64>,
    pub blks_read: Option<i64>,
    pub blks_hit: Option<i64>,
    pub tup_returned: Option<i64>,
    pub tup_fetched: Option<i64>,
    pub rows_written: Option<i64>,
    pub deadlocks: Option<i64>,
    pub temp_files: Option<i64>,
    pub temp_bytes: Option<i64>,
    /// Milliseconds backends spent *executing* since the reset (14+).
    pub active_time_ms: Option<f64>,
    /// Milliseconds of session lifetime since the reset (14+).
    pub session_time_ms: Option<f64>,
    /// When the counters were last zeroed. A change here invalidates every delta.
    pub stats_reset: Option<String>,
}

impl DbHealthSample {
    fn read_row(row: &Row) -> Self {
        Self {
            taken_at: DateTimeAsMicroseconds::now(),
            db_size_bytes: opt_i64(row, "db_size_bytes"),
            num_backends: opt_i64(row, "num_backends"),
            commits: opt_i64(row, "commits"),
            rollbacks: opt_i64(row, "rollbacks"),
            blks_read: opt_i64(row, "blks_read"),
            blks_hit: opt_i64(row, "blks_hit"),
            tup_returned: opt_i64(row, "tup_returned"),
            tup_fetched: opt_i64(row, "tup_fetched"),
            rows_written: opt_i64(row, "rows_written"),
            deadlocks: opt_i64(row, "deadlocks"),
            temp_files: opt_i64(row, "temp_files"),
            temp_bytes: opt_i64(row, "temp_bytes"),
            active_time_ms: opt_f64(row, "active_time_ms"),
            session_time_ms: opt_f64(row, "session_time_ms"),
            stats_reset: opt_timestamp(row, "stats_reset"),
        }
    }
}

stats_row!(DbHealthSample);

/// What happened in the window between the last two samples.
///
/// `None` on the very first tick, and again after a `pg_stat_reset()` — see
/// [`DbHealth::new`].
#[derive(Debug, Clone)]
pub struct DbHealthRates {
    pub window_secs: f64,
    pub commits_per_sec: Option<f64>,
    pub rollbacks_per_sec: Option<f64>,
    pub rows_written_per_sec: Option<f64>,
    pub blks_read_per_sec: Option<f64>,
    /// Cache hit ratio **over the window**, not over all time. The lifetime one
    /// is nearly always a flattering 0.99 because it is dominated by whatever the
    /// database did on the day it was last reset.
    pub cache_hit_ratio: Option<f64>,
    /// Average number of backends executing at any instant during the window
    /// (Δ`active_time` / Δwall-clock).
    ///
    /// **This is as close to CPU as Postgres gets.** It is backend-seconds per
    /// second: 0.2 means the database was executing something 20% of the time,
    /// 3.0 means three backends were busy on average. It is not a percentage of
    /// the host's CPU — it counts time spent waiting on disk and locks as busy
    /// too, and it knows nothing about the other processes on the machine. A real
    /// CPU figure needs an agent on the host; the catalog does not have one.
    pub busy_backends: Option<f64>,
}

/// The `pg_stat_database` card: lifetime counters, plus what moved since the
/// previous tick.
#[derive(Debug, Clone)]
pub struct DbHealth {
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
    /// Since the last stats reset.
    pub lifetime_cache_hit_ratio: Option<f64>,
    pub stats_reset: Option<String>,
    /// `None` while the server predates 14.
    pub active_time_ms: Option<f64>,
    pub session_time_ms: Option<f64>,
    pub rates: Option<DbHealthRates>,
}

/// Δ between two cumulative counters, or `None` if either side is missing or the
/// counter went backwards.
///
/// Backwards means the statistics were reset between the two samples (or the
/// database was recreated). Reporting the raw difference would print a large
/// negative rate; reporting zero would claim the database was idle through a
/// window it may have been hammered in. Neither is true, so the answer is "not
/// known for this window", and the next tick recovers on its own.
fn delta_i64(current: Option<i64>, previous: Option<i64>) -> Option<i64> {
    let (current, previous) = (current?, previous?);

    if current < previous {
        return None;
    }

    Some(current - previous)
}

fn delta_f64(current: Option<f64>, previous: Option<f64>) -> Option<f64> {
    let (current, previous) = (current?, previous?);

    if current < previous {
        return None;
    }

    Some(current - previous)
}

fn per_sec(delta: Option<i64>, window_secs: f64) -> Option<f64> {
    Some(delta? as f64 / window_secs)
}

fn hit_ratio(hit: Option<i64>, read: Option<i64>) -> Option<f64> {
    let total = hit? + read?;

    if total <= 0 {
        return None;
    }

    Some(hit? as f64 / total as f64)
}

impl DbHealth {
    pub fn new(current: &DbHealthSample, previous: Option<&DbHealthSample>) -> Self {
        Self {
            db_size_bytes: current.db_size_bytes,
            num_backends: current.num_backends,
            commits: current.commits,
            rollbacks: current.rollbacks,
            deadlocks: current.deadlocks,
            temp_files: current.temp_files,
            temp_bytes: current.temp_bytes,
            tup_returned: current.tup_returned,
            tup_fetched: current.tup_fetched,
            rows_written: current.rows_written,
            lifetime_cache_hit_ratio: hit_ratio(current.blks_hit, current.blks_read),
            stats_reset: current.stats_reset.clone(),
            active_time_ms: current.active_time_ms,
            session_time_ms: current.session_time_ms,
            rates: previous.and_then(|previous| rates(current, previous)),
        }
    }
}

fn rates(current: &DbHealthSample, previous: &DbHealthSample) -> Option<DbHealthRates> {
    // A reset between the samples makes every counter incomparable, not just the
    // ones that visibly went backwards — an idle counter can survive a reset
    // unchanged and would otherwise contribute a plausible-looking zero.
    if current.stats_reset != previous.stats_reset {
        return None;
    }

    // Microseconds, not `get_full_seconds()`: that truncates, so a 5.9-second window
    // would be divided by 5 and every rate on the page would read ~18% high. The
    // error is systematic and always in the same direction, which is worse than
    // noise — it would make an idle database look busier than it is, consistently.
    let window_secs = current
        .taken_at
        .duration_since(previous.taken_at)
        .get_full_micros() as f64
        / 1_000_000.0;

    // Two samples inside the same second would divide by ~0 and produce
    // meaningless spikes.
    if window_secs < 1.0 {
        return None;
    }

    let blks_hit = delta_i64(current.blks_hit, previous.blks_hit);
    let blks_read = delta_i64(current.blks_read, previous.blks_read);

    Some(DbHealthRates {
        window_secs,
        commits_per_sec: per_sec(delta_i64(current.commits, previous.commits), window_secs),
        rollbacks_per_sec: per_sec(delta_i64(current.rollbacks, previous.rollbacks), window_secs),
        rows_written_per_sec: per_sec(
            delta_i64(current.rows_written, previous.rows_written),
            window_secs,
        ),
        blks_read_per_sec: per_sec(blks_read, window_secs),
        cache_hit_ratio: hit_ratio(blks_hit, blks_read),
        busy_backends: delta_f64(current.active_time_ms, previous.active_time_ms)
            .map(|delta_ms| delta_ms / (window_secs * 1000.0)),
    })
}

/// `active_time`/`session_time` exist from 14 on; below that they are selected as
/// typed NULLs so the rest of the row still arrives.
fn build_sql(capabilities: &ServerCapabilities) -> String {
    let (active_time, session_time) = if capabilities.server_version_num >= PG14 {
        ("d.active_time", "d.session_time")
    } else {
        ("NULL::float8", "NULL::float8")
    };

    format!(
        r#"
SELECT
    pg_database_size(current_database())                                  AS db_size_bytes,
    d.numbackends::int8                                                   AS num_backends,
    d.xact_commit                                                         AS commits,
    d.xact_rollback                                                       AS rollbacks,
    d.blks_read                                                           AS blks_read,
    d.blks_hit                                                            AS blks_hit,
    d.tup_returned                                                        AS tup_returned,
    d.tup_fetched                                                         AS tup_fetched,
    d.tup_inserted + d.tup_updated + d.tup_deleted                        AS rows_written,
    d.deadlocks                                                           AS deadlocks,
    d.temp_files                                                          AS temp_files,
    d.temp_bytes                                                          AS temp_bytes,
    {}                                                                    AS active_time_ms,
    {}                                                                    AS session_time_ms,
    d.stats_reset                                                         AS stats_reset
FROM pg_stat_database d
WHERE d.datname = current_database()
"#,
        active_time, session_time
    )
}

pub async fn collect_health(
    postgres: &PostgresAccess,
    capabilities: &ServerCapabilities,
    timeout: Duration,
) -> Result<DbHealthSample, String> {
    let sql = build_sql(capabilities);

    let rows: Vec<DbHealthSample> = postgres
        .query_typed("db_stats/health", sql.as_str(), timeout)
        .await?;

    rows.into_iter().next().ok_or_else(|| {
        "pg_stat_database has no row for the current database — the statistics collector may be \
         disabled (track_counts = off)."
            .to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(at_secs: i64, commits: i64, active_time_ms: f64) -> DbHealthSample {
        DbHealthSample {
            taken_at: DateTimeAsMicroseconds::new(at_secs * 1_000_000),
            db_size_bytes: Some(1024),
            num_backends: Some(3),
            commits: Some(commits),
            rollbacks: Some(0),
            blks_read: Some(10),
            blks_hit: Some(90),
            tup_returned: Some(0),
            tup_fetched: Some(0),
            rows_written: Some(0),
            deadlocks: Some(0),
            temp_files: Some(0),
            temp_bytes: Some(0),
            active_time_ms: Some(active_time_ms),
            session_time_ms: Some(0.0),
            stats_reset: Some("2026-01-01T00:00:00+00:00".to_string()),
        }
    }

    #[test]
    fn the_first_sample_has_no_rates() {
        let health = DbHealth::new(&sample(100, 10, 0.0), None);

        assert!(health.rates.is_none());
        // ...but the lifetime numbers are there straight away.
        assert_eq!(health.commits, Some(10));
        assert_eq!(health.lifetime_cache_hit_ratio, Some(0.9));
    }

    #[test]
    fn rates_come_from_the_difference_between_two_samples() {
        let previous = sample(100, 10, 0.0);
        // 10 seconds later: 30 more commits, 5 seconds of execution.
        let current = sample(110, 40, 5_000.0);

        let rates = DbHealth::new(&current, Some(&previous)).rates.unwrap();

        assert_eq!(rates.window_secs, 10.0);
        assert_eq!(rates.commits_per_sec, Some(3.0));
        // 5s of backend execution in a 10s window -> 0.5 backends busy on average.
        assert_eq!(rates.busy_backends, Some(0.5));
    }

    #[test]
    fn a_stats_reset_drops_the_rates_instead_of_reporting_a_negative_one() {
        let previous = sample(100, 1_000, 900_000.0);
        let mut current = sample(110, 5, 20.0);
        current.stats_reset = Some("2026-06-01T00:00:00+00:00".to_string());

        assert!(DbHealth::new(&current, Some(&previous)).rates.is_none());
    }

    #[test]
    fn a_counter_that_went_backwards_is_dropped_not_negated() {
        // Same stats_reset, but a counter is lower — treat that one figure as
        // unknown rather than printing a negative rate.
        let previous = sample(100, 1_000, 900_000.0);
        let current = sample(110, 5, 900_000.0);

        let rates = DbHealth::new(&current, Some(&previous)).rates.unwrap();

        assert_eq!(rates.commits_per_sec, None);
        assert_eq!(rates.busy_backends, Some(0.0));
    }

    #[test]
    fn two_samples_in_the_same_second_produce_no_rates() {
        let previous = sample(100, 10, 0.0);
        let current = sample(100, 40, 5_000.0);

        assert!(DbHealth::new(&current, Some(&previous)).rates.is_none());
    }

    #[test]
    fn a_server_older_than_14_reports_no_busy_backends() {
        let mut previous = sample(100, 10, 0.0);
        let mut current = sample(110, 40, 0.0);
        previous.active_time_ms = None;
        current.active_time_ms = None;

        let rates = DbHealth::new(&current, Some(&previous)).rates.unwrap();

        // The commit rate still works — only the timing column is missing.
        assert_eq!(rates.commits_per_sec, Some(3.0));
        assert_eq!(rates.busy_backends, None);
    }
}
