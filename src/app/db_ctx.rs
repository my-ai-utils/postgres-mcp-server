use std::sync::Arc;
use std::sync::atomic::AtomicI64;
use std::time::Duration;

use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::settings::{DatabaseMount, DbConnectionSettings, ServerEndpoint, SettingsReader};

/// How much time a single "Enable" click adds. Clicks accumulate — pressing it
/// three times buys 30 minutes.
pub const MCP_WRITES_WINDOW: Duration = Duration::from_secs(600);

/// New expiry after one click: a full window stacked on whatever is left.
///
/// An already-lapsed expiry does not carry over — `current` in the past means
/// the clock restarts from `now`, so a window that closed an hour ago cannot
/// make the next click grant less than a full 10 minutes.
fn extended_until(current: i64, now: i64, window_micros: i64) -> i64 {
    let base = if current > now { current } else { now };
    base + window_micros
}

/// One mounted database: its connection, and the write-access window guarding
/// it.
///
/// The window is deliberately **per database**, not global — each mount is a
/// separate endpoint with a separate connection string and, typically, a
/// separate blast radius. Granting writes on a scratch database must not open
/// production along with it.
pub struct DbContext {
    /// Normalized mount path, e.g. `/mcp`. Doubles as the database's id on the
    /// wire and in the request log. `Arc` so tagging a log entry with it costs
    /// a refcount rather than a copy of the string.
    pub path: Arc<String>,
    /// What this database is, from the settings file — the UI subtitle, and the
    /// MCP endpoint's instructions to the agent.
    pub description: String,

    /// Which Postgres server this mount talks to, read off the connection string
    /// once at startup.
    ///
    /// Several mounts routinely share one server, and the operator never states
    /// that anywhere — so it is derived, and it is what the UI groups and switches
    /// by. Resolved here rather than per request because the connection string is
    /// re-read live and a parse per API call would be both wasteful and able to
    /// change a mount's server identity underneath the UI mid-session.
    pub server: ServerEndpoint,

    pub postgres: crate::postgres::PostgresAccess,

    /// Last statistics collected for **this** database.
    ///
    /// Per database rather than one shared cache, for the same reason the write
    /// window is: the sections are scoped to a single connection — its server
    /// version, its privileges, its tables — and a mount whose database is
    /// unreachable must not blank the numbers of the ones that are up.
    pub stats: crate::db_stats::DbStatsCache,

    /// The longest-running statements seen on this database since the last hourly
    /// flush. Filled from the 5-second ticks, because `pg_stat_activity` keeps no
    /// history of its own — see [`crate::db_stats::LongestSeenInHour`].
    pub longest_seen: crate::db_stats::LongestSeenInHour,

    /// Expiry (`unix_microseconds`) of this database's write-access window; `0`
    /// means disabled. Stored as the deadline rather than a flag plus a timer,
    /// so it expires lazily on read and needs no background task.
    ///
    /// Deliberately runtime-only and never persisted: a restart always comes up
    /// with writes disabled.
    mcp_writes_enabled_until: AtomicI64,
}

impl DbContext {
    pub async fn new(mount: DatabaseMount, settings: Arc<SettingsReader>) -> Self {
        let server = mount.server();

        let conn_settings = Arc::new(DbConnectionSettings::new(settings, &mount));

        // The Postgres `application_name` carries the mount path, so a session
        // on the server side says which endpoint opened it.
        let app_name = format!("{}{}", crate::app::APP_NAME, mount.path);

        Self {
            path: Arc::new(mount.path),
            description: mount.description,
            server,
            postgres: crate::postgres::PostgresAccess::new(app_name, conn_settings).await,
            stats: crate::db_stats::DbStatsCache::new(),
            longest_seen: crate::db_stats::LongestSeenInHour::new(),
            mcp_writes_enabled_until: AtomicI64::new(0),
        }
    }

    /// Adds [`MCP_WRITES_WINDOW`] to this database's write window, opening it if
    /// it was closed. Each call stacks, so three calls grant 30 minutes.
    pub fn enable_mcp_writes(&self) {
        let now = DateTimeAsMicroseconds::now().unix_microseconds;
        let window = MCP_WRITES_WINDOW.as_micros() as i64;

        // Read-modify-write, so two clicks landing at once both count instead of
        // one silently overwriting the other.
        let _ = self.mcp_writes_enabled_until.fetch_update(
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
            |current| Some(extended_until(current, now, window)),
        );
    }

    /// Resets the window to closed, immediately.
    pub fn disable_mcp_writes(&self) {
        self.mcp_writes_enabled_until
            .store(0, std::sync::atomic::Ordering::SeqCst);
    }

    /// `Some(expiry)` while the window is still open, otherwise `None`.
    pub fn mcp_writes_enabled_until(&self) -> Option<DateTimeAsMicroseconds> {
        let until = self
            .mcp_writes_enabled_until
            .load(std::sync::atomic::Ordering::Relaxed);

        if until > DateTimeAsMicroseconds::now().unix_microseconds {
            Some(DateTimeAsMicroseconds::new(until))
        } else {
            None
        }
    }

    pub fn is_mcp_write_enabled(&self) -> bool {
        self.mcp_writes_enabled_until().is_some()
    }

    pub fn mcp_writes_remaining_secs(&self) -> Option<u64> {
        let until = self.mcp_writes_enabled_until()?;
        let left = until.duration_since(DateTimeAsMicroseconds::now());
        Some(left.get_full_seconds().max(0) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 10 minutes in microseconds.
    const W: i64 = 600_000_000;
    const NOW: i64 = 1_700_000_000_000_000;

    #[test]
    fn first_click_opens_a_full_window_from_now() {
        assert_eq!(extended_until(0, NOW, W), NOW + W);
    }

    #[test]
    fn each_click_adds_another_window() {
        let one = extended_until(0, NOW, W);
        let two = extended_until(one, NOW, W);
        let three = extended_until(two, NOW, W);

        assert_eq!(one, NOW + W);
        assert_eq!(two, NOW + 2 * W);
        assert_eq!(three, NOW + 3 * W);
    }

    #[test]
    fn clicking_mid_window_stacks_on_the_remainder() {
        // 4 minutes left -> clicking makes it 14, not 10.
        let four_minutes_left = NOW + 240_000_000;
        assert_eq!(
            extended_until(four_minutes_left, NOW, W),
            NOW + 240_000_000 + W
        );
    }

    #[test]
    fn a_lapsed_window_does_not_carry_over() {
        // Expired an hour ago: the next click must still grant a full window,
        // never a negative or shortened one.
        let long_expired = NOW - 3_600_000_000;
        assert_eq!(extended_until(long_expired, NOW, W), NOW + W);
    }
}
