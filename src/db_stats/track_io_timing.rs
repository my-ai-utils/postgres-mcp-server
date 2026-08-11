//! Turning `track_io_timing` on and off from the admin UI.
//!
//! # Why this is a fixed statement and not a SQL passthrough
//!
//! The two statements below are `&'static str` and never interpolate anything. The
//! request body carries a mount path and a boolean, and the boolean selects between
//! two literals that are written out in full here. Nothing a client sends reaches
//! the parser, so this endpoint cannot be turned into an arbitrary-SQL endpoint by
//! anyone who can reach the port.
//!
//! # What it changes, and how far that reaches
//!
//! `ALTER SYSTEM` writes `postgresql.auto.conf` and applies to the **whole Postgres
//! server** — every database on that cluster, including ones this server is not
//! configured for. It needs superuser, and it is a reload rather than a restart. The
//! UI says all of this in the confirmation before the request is sent.
//!
//! # Why the setting is read back
//!
//! `ALTER SYSTEM` only edits a file; the value takes effect on reload. Reporting
//! success from "the statements did not error" would claim the switch was thrown
//! when it might not have been — so the new value is read from the live session and
//! returned, and the UI shows what the *server* says rather than what was asked for.

use std::time::Duration;

use my_postgres::tokio_postgres::Row;

use crate::postgres::{PostgresAccess, opt_bool, stats_row};

/// Generous compared with the collector's 5 seconds: this runs once, on a click, and
/// writing `postgresql.auto.conf` plus a config reload is more work than a catalog
/// read.
const TIMEOUT: Duration = Duration::from_secs(10);

const ENABLE: &str = "ALTER SYSTEM SET track_io_timing = on";
const DISABLE: &str = "ALTER SYSTEM SET track_io_timing = off";
const RELOAD: &str = "SELECT pg_reload_conf()";

/// The value the server reports for itself, after the change.
struct TrackIoTiming {
    enabled: bool,
}

impl TrackIoTiming {
    fn read_row(row: &Row) -> Self {
        Self {
            enabled: opt_bool(row, "enabled").unwrap_or_default(),
        }
    }
}

stats_row!(TrackIoTiming);

const READ_BACK: &str = "SELECT current_setting('track_io_timing') = 'on' AS enabled";

/// Applies the change and returns what the server says afterwards.
///
/// `Err` carries the driver's own message verbatim. That matters here more than
/// usual: the two failures worth telling apart — "must be superuser" and "cannot run
/// inside a transaction block" — are both things the operator can act on, and only
/// Postgres knows which one happened.
pub async fn set_track_io_timing(
    postgres: &PostgresAccess,
    enabled: bool,
) -> Result<bool, String> {
    postgres
        .execute_statement(if enabled { ENABLE } else { DISABLE }, TIMEOUT)
        .await?;

    // Without the reload the value sits in postgresql.auto.conf unread, and the read
    // back below would report the old one — correctly, which would look like the
    // change silently failed.
    postgres.execute_statement(RELOAD, TIMEOUT).await?;

    let rows: Vec<TrackIoTiming> = postgres
        .query_typed("settings/track_io_timing", READ_BACK, TIMEOUT)
        .await?;

    rows.into_iter()
        .next()
        .map(|row| row.enabled)
        .ok_or_else(|| "The server did not report its track_io_timing setting back.".to_string())
}

/// The SQL this endpoint runs, for the audit log and for the UI to show.
pub fn statements(enabled: bool) -> String {
    format!("{};\n{};", if enabled { ENABLE } else { DISABLE }, RELOAD)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_statements_are_fixed_and_carry_no_input() {
        // The boolean selects between two literals; nothing is interpolated. If this
        // ever needs a value from the request, it does not belong on this path.
        assert_eq!(statements(true), "ALTER SYSTEM SET track_io_timing = on;\nSELECT pg_reload_conf();");
        assert_eq!(statements(false), "ALTER SYSTEM SET track_io_timing = off;\nSELECT pg_reload_conf();");
    }
}
