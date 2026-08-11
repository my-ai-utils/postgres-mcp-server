//! The longest-running statements **observed during an hour**.
//!
//! `pg_stat_activity` is a point-in-time view: it shows what is executing right
//! now and keeps no history. Reading it once an hour would therefore record only
//! whatever happened to be running in that one second and miss every slow query
//! in between — which is exactly the opposite of what "top 5 longest per hour" is
//! for.
//!
//! So the hour is assembled from the 5-second ticks the collector already makes.
//! Each tick contributes what it saw; this keeps the worst of them and the hourly
//! timer flushes the top 5 to history and starts a new hour.

use std::collections::HashMap;

use parking_lot::Mutex;

use super::LongRunningQuery;

/// How many are kept per hour.
pub const TOP_N: usize = 5;

/// Ceiling on distinct executions tracked within one hour, so a database churning
/// through short statements cannot grow this without bound between flushes. Well
/// above [`TOP_N`] because an execution's duration only grows while it runs: the
/// candidate set has to stay wide enough that a query which is currently 2 seconds
/// old is still present when it reaches 200.
const MAX_TRACKED: usize = 512;

/// The longest sighting of one execution.
#[derive(Debug, Clone)]
pub struct LongestSeen {
    pub pid: Option<i32>,
    pub query_start: Option<String>,
    pub user_name: Option<String>,
    pub application_name: Option<String>,
    pub wait: Option<String>,
    /// The largest `running_secs` any tick saw for this execution — a statement is
    /// observed repeatedly as it runs, and only its final, longest sighting is
    /// interesting.
    pub running_secs: f64,
    pub query: Option<String>,
}

/// Per-database accumulator for the current hour.
pub struct LongestSeenInHour {
    /// Keyed by execution, so one statement seen by twelve consecutive ticks is one
    /// entry that grows, not twelve entries at increasing durations.
    ///
    /// Written by the fast timer and drained by the hourly one, both off the
    /// request path; the lock is only ever held for the map operation itself.
    state: Mutex<HashMap<ExecutionKey, LongestSeen>>,
}

/// `(pid, query_start)` for statements that report both, and a synthetic
/// per-sighting key for the ones that do not — see [`LongestSeenInHour::observe`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ExecutionKey {
    Execution(i32, String),
    /// An execution this account cannot fully see. Keyed by its text and rounded
    /// duration rather than merged with anything, since there is no id to merge on.
    Anonymous(String, i64),
}

impl LongestSeenInHour {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(HashMap::new()),
        }
    }

    /// Folds one tick's sightings in.
    pub fn observe(&self, seen: &[LongRunningQuery]) {
        let mut state = self.state.lock();

        for query in seen {
            let Some(running_secs) = query.running_secs else {
                // Without a duration there is nothing to rank it by.
                continue;
            };

            let key = match query.execution_key() {
                Some((pid, query_start)) => ExecutionKey::Execution(pid, query_start),
                None => ExecutionKey::Anonymous(
                    query.query.clone().unwrap_or_default(),
                    running_secs as i64,
                ),
            };

            let entry = state.entry(key).or_insert_with(|| LongestSeen {
                pid: query.pid,
                query_start: query.query_start.clone(),
                user_name: query.user_name.clone(),
                application_name: query.application_name.clone(),
                wait: query.wait.clone(),
                running_secs,
                query: query.query.clone(),
            });

            // Keep the longest sighting, and the wait state that went with it — a
            // query that ended up blocked is more informative than the moment it
            // was still running freely.
            if running_secs >= entry.running_secs {
                entry.running_secs = running_secs;
                entry.wait = query.wait.clone();
            }
        }

        // Only trim once the map is genuinely large, and trim down to the cap rather
        // than to TOP_N: dropping to 5 every time would evict queries that are
        // merely young, and a young query is the one most likely to become the
        // hour's longest.
        if state.len() > MAX_TRACKED {
            let mut ranked: Vec<_> = state.drain().collect();
            ranked.sort_by(|left, right| sort_by_duration(&left.1, &right.1));
            ranked.truncate(MAX_TRACKED);
            *state = ranked.into_iter().collect();
        }
    }

    /// The hour's top [`TOP_N`], longest first, and starts a fresh hour.
    ///
    /// Draining rather than copying is deliberate: "top 5 of this hour" must not
    /// inherit last hour's winner, or a single very slow query would sit at the top
    /// of every hour that followed it.
    pub fn take_top(&self) -> Vec<LongestSeen> {
        let mut state = self.state.lock();

        let mut ranked: Vec<LongestSeen> = state.drain().map(|(_, value)| value).collect();

        drop(state);

        ranked.sort_by(sort_by_duration);
        ranked.truncate(TOP_N);
        ranked
    }
}

/// Longest first. `partial_cmp` cannot fail here — the durations come from
/// `EXTRACT(EPOCH ...)` and a NaN would have to survive the driver — but the
/// fallback keeps the sort total rather than risking a panic on a value from the
/// network.
fn sort_by_duration(left: &LongestSeen, right: &LongestSeen) -> std::cmp::Ordering {
    right
        .running_secs
        .partial_cmp(&left.running_secs)
        .unwrap_or(std::cmp::Ordering::Equal)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(pid: i32, start: &str, secs: f64, sql: &str) -> LongRunningQuery {
        LongRunningQuery {
            pid: Some(pid),
            query_start: Some(start.to_string()),
            user_name: Some("u".to_string()),
            application_name: None,
            state: Some("active".to_string()),
            wait: None,
            running_secs: Some(secs),
            query: Some(sql.to_string()),
        }
    }

    #[test]
    fn one_execution_seen_by_many_ticks_is_one_entry_at_its_longest() {
        let hour = LongestSeenInHour::new();

        // The same statement, observed by three consecutive ticks as it runs.
        for secs in [5.0, 10.0, 17.5] {
            hour.observe(&[seen(42, "2026-08-11T10:00:00+00:00", secs, "SELECT 1")]);
        }

        let top = hour.take_top();

        assert_eq!(top.len(), 1);
        assert_eq!(top[0].running_secs, 17.5);
    }

    #[test]
    fn the_same_pid_running_a_new_statement_is_a_new_execution() {
        let hour = LongestSeenInHour::new();

        // Postgres reuses the backend, so pid alone would merge these two.
        hour.observe(&[seen(42, "2026-08-11T10:00:00+00:00", 30.0, "SELECT a")]);
        hour.observe(&[seen(42, "2026-08-11T10:05:00+00:00", 4.0, "SELECT b")]);

        let top = hour.take_top();

        assert_eq!(top.len(), 2);
        assert_eq!(top[0].running_secs, 30.0);
        assert_eq!(top[1].running_secs, 4.0);
    }

    #[test]
    fn top_is_longest_first_and_capped() {
        let hour = LongestSeenInHour::new();

        for i in 0..12 {
            hour.observe(&[seen(
                i,
                "2026-08-11T10:00:00+00:00",
                i as f64,
                "SELECT 1",
            )]);
        }

        let top = hour.take_top();

        assert_eq!(top.len(), TOP_N);
        assert_eq!(top[0].running_secs, 11.0);
        assert_eq!(top[TOP_N - 1].running_secs, 7.0);
    }

    #[test]
    fn taking_the_top_starts_a_fresh_hour() {
        let hour = LongestSeenInHour::new();

        hour.observe(&[seen(1, "2026-08-11T10:00:00+00:00", 900.0, "SELECT slow")]);
        assert_eq!(hour.take_top().len(), 1);

        // Without draining, that 900-second query would head every following hour.
        assert!(hour.take_top().is_empty());

        hour.observe(&[seen(2, "2026-08-11T11:00:00+00:00", 1.0, "SELECT fast")]);
        let next = hour.take_top();
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].running_secs, 1.0);
    }

    #[test]
    fn a_sighting_without_a_duration_is_ignored() {
        let hour = LongestSeenInHour::new();

        let mut no_duration = seen(1, "2026-08-11T10:00:00+00:00", 0.0, "SELECT 1");
        no_duration.running_secs = None;

        hour.observe(&[no_duration]);

        assert!(hour.take_top().is_empty());
    }

    #[test]
    fn the_wait_state_follows_the_longest_sighting() {
        let hour = LongestSeenInHour::new();

        let mut running = seen(7, "2026-08-11T10:00:00+00:00", 3.0, "UPDATE t");
        hour.observe(&[running.clone()]);

        // Later it is longer and blocked — that is the version worth keeping.
        running.running_secs = Some(60.0);
        running.wait = Some("Lock: transactionid".to_string());
        hour.observe(&[running]);

        let top = hour.take_top();
        assert_eq!(top[0].running_secs, 60.0);
        assert_eq!(top[0].wait.as_deref(), Some("Lock: transactionid"));
    }

    #[test]
    fn tracking_is_bounded_but_keeps_the_longest() {
        let hour = LongestSeenInHour::new();

        for i in 0..(MAX_TRACKED as i32 + 200) {
            hour.observe(&[seen(
                i,
                "2026-08-11T10:00:00+00:00",
                i as f64,
                "SELECT 1",
            )]);
        }

        let top = hour.take_top();

        // The trim drops the shortest, never the longest.
        assert_eq!(top.len(), TOP_N);
        assert_eq!(top[0].running_secs, (MAX_TRACKED as f64) + 199.0);
    }
}
