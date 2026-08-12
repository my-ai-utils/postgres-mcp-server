//! Per-minute throughput: how many statements ran, how long they took on average,
//! and the longest one.
//!
//! # Where each number comes from, and how far to trust it
//!
//! The window is the slow tick, which is a minute. Two different sources, because no
//! single one answers all three questions:
//!
//! | Figure | Source | Exactness |
//! |---|---|---|
//! | **calls** | Σ Δ`pg_stat_statements.calls` | exact — a counter of every execution |
//! | **average** | Σ Δ`total_exec_time` ÷ Σ Δ`calls` | exact for the window |
//! | **longest** | sampled `pg_stat_activity`, every 5 s | a **floor**, see below |
//!
//! The first two cover **every statement the database ran**, not just the agent's —
//! they come from the extension's own counters, which count everything.
//!
//! # The longest is a floor, not a maximum
//!
//! `pg_stat_statements` keeps `max_exec_time` since its last reset, not per window,
//! so it cannot answer "the longest in *this* minute". Sampling `pg_stat_activity`
//! can, but only sees statements that were still running at a sample instant: one
//! that starts and finishes inside a 5-second gap is invisible.
//!
//! That trade is the right way round — the misses are the fast ones — but it does
//! mean the reported longest can only be too *low*, never too high, and everything
//! that publishes it says so.
//!
//! One exact signal is kept alongside it: when a statement's lifetime
//! `max_exec_time` *increases* within the window, a new slowest execution definitely
//! completed in it, and its duration is known precisely. That is
//! [`MinuteThroughput::slowest_finished_ms`] — present when a record was broken,
//! absent otherwise, and never a guess.

use super::LongestSeen;

/// One minute of traffic.
#[derive(Debug, Clone, Default)]
pub struct MinuteThroughput {
    /// The window these figures cover. Nominally 60 seconds, but reported because a
    /// tick that ran late makes "per minute" a lie the reader cannot otherwise see.
    pub window_secs: f64,
    /// Statements that completed in the window, across the whole database.
    pub calls: Option<i64>,
    /// Calls per second, for comparing windows of different lengths.
    pub calls_per_sec: Option<f64>,
    /// Total execution time divided by calls.
    pub avg_exec_ms: Option<f64>,
    /// Total execution time in the window — what the average is derived from, kept
    /// because it is the figure that actually says how much work was done.
    pub total_exec_ms: Option<f64>,
    /// Longest execution *observed* by the 5-second sampler. A floor: see the module
    /// docs.
    pub longest_secs: Option<f64>,
    /// The text of that statement, when this account may read it.
    pub longest_query: Option<String>,
    /// A new lifetime maximum set during the window — exact when present.
    pub slowest_finished_ms: Option<f64>,
    /// The statement that set it.
    pub slowest_finished_query: Option<String>,
}

impl MinuteThroughput {
    pub fn new(
        window_secs: f64,
        calls: Option<i64>,
        total_exec_ms: Option<f64>,
        longest: Option<&LongestSeen>,
        record: Option<(f64, Option<String>)>,
    ) -> Self {
        Self {
            window_secs,
            calls,
            calls_per_sec: calls
                .filter(|_| window_secs > 0.0)
                .map(|calls| calls as f64 / window_secs),
            // Guarded against a zero denominator: a minute with no calls has no
            // average, and 0/0 would reach the UI as NaN.
            avg_exec_ms: calls.zip(total_exec_ms).and_then(|(calls, total)| {
                if calls <= 0 {
                    return None;
                }

                Some(total / calls as f64)
            }),
            total_exec_ms,
            longest_secs: longest.map(|longest| longest.running_secs),
            longest_query: longest.and_then(|longest| longest.query.clone()),
            slowest_finished_ms: record.as_ref().map(|(ms, _)| *ms),
            slowest_finished_query: record.and_then(|(_, query)| query),
        }
    }
}

// The stored shape lives in `super::store` as `MinuteThroughputSample` — it truncates
// the query texts, which only the store cares about. Whether a window is "about a
// minute" is decided by the UI, which is the only place that renders the label.

#[cfg(test)]
mod tests {
    use super::*;

    fn longest(secs: f64) -> LongestSeen {
        LongestSeen {
            pid: Some(1),
            query_start: Some("2026-08-11T10:00:00+00:00".to_string()),
            user_name: None,
            application_name: None,
            wait: None,
            running_secs: secs,
            query: Some("SELECT slow".to_string()),
        }
    }

    #[test]
    fn the_average_is_total_time_over_calls() {
        let minute = MinuteThroughput::new(60.0, Some(120), Some(600.0), None, None);

        assert_eq!(minute.avg_exec_ms, Some(5.0));
        assert_eq!(minute.calls_per_sec, Some(2.0));
    }

    #[test]
    fn a_minute_with_no_calls_has_no_average_rather_than_nan() {
        let minute = MinuteThroughput::new(60.0, Some(0), Some(0.0), None, None);

        assert_eq!(minute.avg_exec_ms, None);
        assert_eq!(minute.calls_per_sec, Some(0.0));
    }

    #[test]
    fn the_longest_comes_from_the_sampler_and_the_record_from_the_extension() {
        let minute = MinuteThroughput::new(
            60.0,
            Some(10),
            Some(100.0),
            Some(&longest(42.5)),
            Some((1_250.0, Some("UPDATE t".to_string()))),
        );

        // Observed by sampling — a floor.
        assert_eq!(minute.longest_secs, Some(42.5));
        assert_eq!(minute.longest_query.as_deref(), Some("SELECT slow"));
        // A new lifetime maximum — exact.
        assert_eq!(minute.slowest_finished_ms, Some(1_250.0));
        assert_eq!(minute.slowest_finished_query.as_deref(), Some("UPDATE t"));
    }

    #[test]
    fn nothing_observed_reports_nothing_rather_than_zero() {
        // A minute in which no statement ran long enough to be sampled must not claim
        // the longest query took 0 seconds.
        let minute = MinuteThroughput::new(60.0, Some(5), Some(10.0), None, None);

        assert_eq!(minute.longest_secs, None);
        assert_eq!(minute.slowest_finished_ms, None);
    }
}
