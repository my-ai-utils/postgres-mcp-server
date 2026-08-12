//! Installing `pg_stat_statements` from the admin UI.
//!
//! # Two steps, and only one of them can be finished here
//!
//! ```sql
//! ALTER SYSTEM SET shared_preload_libraries = '…,pg_stat_statements';  -- needs a RESTART
//! CREATE EXTENSION pg_stat_statements;                                 -- takes effect at once
//! ```
//!
//! `shared_preload_libraries` is a postmaster-level setting: a reload does not pick
//! it up, only a full restart does, and this server has no business restarting
//! anyone's database. So the preload step ends in "now restart Postgres", and the
//! create step is the one that completes on its own.
//!
//! Often the library is already preloaded — it is on by default on RDS and on many
//! packaged installs — and then `CREATE EXTENSION` alone is the whole job.
//!
//! # Two ways to break a server, both guarded
//!
//! 1. **Overwriting somebody else's libraries.** `shared_preload_libraries` is a
//!    list. Setting it to `pg_stat_statements` on a server running `pg_cron` or
//!    `timescaledb` disables those at the next restart — a failure that appears hours
//!    later, during a maintenance window, with no obvious cause. So the current value
//!    is read and **appended to**, never replaced.
//! 2. **Naming a library that is not on disk.** Postgres **refuses to start** if
//!    `shared_preload_libraries` names something it cannot load. If the contrib
//!    package is not installed, writing this setting turns a working server into one
//!    that will not come back up after its next restart. So
//!    [`ExtensionAvailability::available`] is checked first, from
//!    `pg_available_extensions`, and the endpoint refuses when it is false.
//!
//! # Why the appended value is interpolated, and why that is safe here
//!
//! Unlike [`super::track_io_timing`], this statement cannot be a fixed string: the
//! new value has to contain the old one. `ALTER SYSTEM SET` takes a literal, not an
//! expression, so there is no `quote_literal` to lean on either.
//!
//! The value comes from the server's own configuration rather than from a request,
//! and it is validated against [`is_safe_library_list`] before it is used — a library
//! name is a file name, so anything outside that character set means something is
//! wrong and the change is refused rather than escaped.

use std::time::Duration;

use my_postgres::tokio_postgres::Row;

use crate::postgres::{PostgresAccess, opt_bool, opt_string, stats_row};

const TIMEOUT: Duration = Duration::from_secs(10);

const EXTENSION: &str = "pg_stat_statements";

/// Fixed, and the common case: the library is already preloaded and this is the whole
/// installation.
const CREATE: &str = "CREATE EXTENSION IF NOT EXISTS pg_stat_statements";

/// What the server can tell us about installing it.
#[derive(Debug, Clone)]
pub struct ExtensionAvailability {
    /// Present in `pg_available_extensions` — the control file is on disk, so the
    /// contrib package is installed and the library can actually be loaded.
    ///
    /// **The precondition for touching `shared_preload_libraries` at all**: naming a
    /// library that is not there stops Postgres from starting.
    pub available: bool,
    /// Already listed in `shared_preload_libraries`, so `CREATE EXTENSION` is enough
    /// and no restart is needed.
    pub preloaded: bool,
    /// The current list, verbatim, so the new value can be appended to it.
    pub current_preload: String,
}

impl ExtensionAvailability {
    fn read_row(row: &Row) -> Self {
        Self {
            available: opt_bool(row, "available").unwrap_or_default(),
            preloaded: opt_bool(row, "preloaded").unwrap_or_default(),
            current_preload: opt_string(row, "current_preload").unwrap_or_default(),
        }
    }
}

stats_row!(ExtensionAvailability);

/// The `LIKE '%pg_stat_statements%'` is deliberately loose: the setting is a
/// comma-separated list whose spacing is up to whoever wrote it, and a false positive
/// here only means the UI offers `CREATE EXTENSION` — which is harmless and
/// idempotent — whereas a false negative would offer to append a library that is
/// already there.
const PROBE: &str = r#"
SELECT
    EXISTS (SELECT 1 FROM pg_available_extensions WHERE name = 'pg_stat_statements')  AS available,
    current_setting('shared_preload_libraries') LIKE '%pg_stat_statements%'           AS preloaded,
    current_setting('shared_preload_libraries')                                       AS current_preload
"#;

pub async fn probe(postgres: &PostgresAccess) -> Result<ExtensionAvailability, String> {
    let rows: Vec<ExtensionAvailability> = postgres
        .query_typed("settings/extension_probe", PROBE, TIMEOUT)
        .await?;

    rows.into_iter()
        .next()
        .ok_or_else(|| "The extension probe returned no row.".to_string())
}

/// A library list is a list of file names. Anything else — a quote, a semicolon, a
/// newline — means the value is not what this code thinks it is, and the change is
/// refused rather than escaped and hoped for.
pub fn is_safe_library_list(value: &str) -> bool {
    // `/` and `$` are in the set because a library may legitimately be written as a
    // path — `$libdir/auto_explain` is valid. The character that actually matters is
    // the single quote, which would end the literal; everything here is chosen to
    // exclude it and its friends rather than to enumerate what Postgres accepts.
    value.chars().all(|c| {
        c.is_ascii_alphanumeric() || matches!(c, ',' | ' ' | '_' | '-' | '.' | '$' | '/' | '\t')
    })
}

/// The new `shared_preload_libraries` value: the current one with the extension
/// appended.
///
/// `None` when it is already there — appending twice is not harmful, but a no-op
/// change that reports "now restart Postgres" would send someone into a maintenance
/// window for nothing.
pub fn appended_preload(current: &str) -> Option<String> {
    let trimmed = current.trim();

    if trimmed
        .split(',')
        .any(|library| library.trim() == EXTENSION)
    {
        return None;
    }

    if trimmed.is_empty() {
        return Some(EXTENSION.to_string());
    }

    Some(format!("{},{}", trimmed, EXTENSION))
}

/// What the caller asked for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SetupAction {
    /// `CREATE EXTENSION` — finishes the job, no restart.
    Create,
    /// Append to `shared_preload_libraries` — needs a restart afterwards.
    Preload,
}

/// What happened, in the operator's terms.
#[derive(Debug, Clone)]
pub struct SetupOutcome {
    pub done: bool,
    /// True when Postgres must be restarted before this takes effect.
    pub restart_required: bool,
    pub message: String,
    /// The statement that ran, for the audit log.
    pub sql: String,
}

pub async fn run(
    postgres: &PostgresAccess,
    action: SetupAction,
) -> Result<SetupOutcome, String> {
    let state = probe(postgres).await?;

    if !state.available {
        return Ok(SetupOutcome {
            done: false,
            restart_required: false,
            message: "pg_stat_statements is not available on this server — it is part of the \
                      postgresql-contrib package, which is not installed. Install that package \
                      on the database host first; setting shared_preload_libraries to a library \
                      that is not on disk would stop Postgres from starting."
                .to_string(),
            sql: String::new(),
        });
    }

    match action {
        SetupAction::Create => {
            postgres.execute_statement(CREATE, TIMEOUT).await?;

            Ok(SetupOutcome {
                done: true,
                restart_required: false,
                // Creating the extension without the library preloaded succeeds, but
                // every read of the view then errors. Saying so here is the difference
                // between "installed" and "installed and working".
                message: if state.preloaded {
                    "pg_stat_statements is installed. The first figures appear after the next \
                     collection tick."
                        .to_string()
                } else {
                    "The extension was created, but the library is still not in \
                     shared_preload_libraries — reading it will fail until that is set and \
                     Postgres is restarted."
                        .to_string()
                },
                sql: format!("{};", CREATE),
            })
        }

        SetupAction::Preload => {
            let Some(new_value) = appended_preload(state.current_preload.as_str()) else {
                return Ok(SetupOutcome {
                    done: true,
                    restart_required: false,
                    message: "pg_stat_statements is already in shared_preload_libraries."
                        .to_string(),
                    sql: String::new(),
                });
            };

            if !is_safe_library_list(new_value.as_str()) {
                return Ok(SetupOutcome {
                    done: false,
                    restart_required: false,
                    message: format!(
                        "shared_preload_libraries currently reads {:?}, which contains characters \
                         a library list should not. Refusing to rewrite it — set it by hand.",
                        state.current_preload
                    ),
                    sql: String::new(),
                });
            }

            let sql = format!(
                "ALTER SYSTEM SET shared_preload_libraries = '{}'",
                new_value
            );

            // Leaked deliberately: `execute_statement` takes `&'static str` precisely
            // so that no caller can pass a built string by accident. This is the one
            // place a value has to be interpolated, the value has just been validated,
            // and one small leak per click is the cost of keeping that signature
            // honest everywhere else.
            let leaked: &'static str = Box::leak(sql.clone().into_boxed_str());

            postgres.execute_statement(leaked, TIMEOUT).await?;

            Ok(SetupOutcome {
                done: true,
                restart_required: true,
                message: format!(
                    "shared_preload_libraries is now '{}'. This does NOT take effect until \
                     Postgres is restarted — a reload is not enough. After the restart, come \
                     back and create the extension.",
                    new_value
                ),
                sql: format!("{};", sql),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_extension_is_appended_to_the_existing_libraries_never_replacing_them() {
        // Replacing would disable pg_cron at the next restart — hours later, in a
        // maintenance window, with no obvious cause.
        assert_eq!(
            appended_preload("pg_cron,timescaledb").as_deref(),
            Some("pg_cron,timescaledb,pg_stat_statements")
        );
    }

    #[test]
    fn an_empty_list_becomes_just_the_extension() {
        assert_eq!(appended_preload("").as_deref(), Some("pg_stat_statements"));
        assert_eq!(appended_preload("   ").as_deref(), Some("pg_stat_statements"));
    }

    #[test]
    fn an_extension_already_present_is_not_appended_twice() {
        // A no-op change that reported "now restart Postgres" would send someone into
        // a maintenance window for nothing.
        assert_eq!(appended_preload("pg_stat_statements"), None);
        assert_eq!(appended_preload("pg_cron, pg_stat_statements"), None);
        assert_eq!(appended_preload("pg_stat_statements , pg_cron"), None);
    }

    #[test]
    fn a_similarly_named_library_is_not_mistaken_for_it() {
        // Matching on the whole element, not a substring.
        assert!(appended_preload("pg_stat_statements_extra").is_some());
        assert!(appended_preload("my_pg_stat_statements").is_some());
    }

    #[test]
    fn a_library_list_is_file_names_and_nothing_else() {
        assert!(is_safe_library_list("pg_cron,timescaledb, pg_stat_statements"));
        assert!(is_safe_library_list("auto_explain"));
        assert!(is_safe_library_list("$libdir/thing"));

        // Anything that could end the literal is refused rather than escaped.
        assert!(!is_safe_library_list("pg_cron'; DROP TABLE users; --"));
        assert!(!is_safe_library_list("pg_cron'"));
        assert!(!is_safe_library_list("pg_cron\nauto_explain"));
    }
}
