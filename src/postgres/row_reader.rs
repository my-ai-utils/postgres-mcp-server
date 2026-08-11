//! Reading columns out of the hand-written statistics queries in
//! [`crate::db_stats`].
//!
//! Every getter here returns `Option`, and a type mismatch reads the same as a
//! SQL `NULL`. That is deliberate rather than lazy: the catalog views these
//! queries hit return NULL in perfectly normal operation —
//!
//! - `pg_stat_user_tables.idx_scan` on a table that was never scanned by index,
//! - `last_vacuum` before the first vacuum,
//! - `pg_stat_activity.query` / `.state` on a backend this account may not
//!   inspect,
//! - `pg_stat_database.active_time` on a server older than 14,
//!
//! — and the sections carry those straight through to the wire as `null` instead
//! of inventing a zero. A zero would read as "nothing happened" when the truth is
//! "this server, or this account, cannot tell you".

use my_postgres::tokio_postgres::Row;

pub fn opt_bool(row: &Row, name: &str) -> Option<bool> {
    row.try_get::<_, Option<bool>>(name).ok().flatten()
}

pub fn opt_i32(row: &Row, name: &str) -> Option<i32> {
    row.try_get::<_, Option<i32>>(name).ok().flatten()
}

pub fn opt_i64(row: &Row, name: &str) -> Option<i64> {
    row.try_get::<_, Option<i64>>(name).ok().flatten()
}

pub fn opt_f64(row: &Row, name: &str) -> Option<f64> {
    row.try_get::<_, Option<f64>>(name).ok().flatten()
}

pub fn opt_string(row: &Row, name: &str) -> Option<String> {
    row.try_get::<_, Option<String>>(name).ok().flatten()
}

/// A `timestamptz` column as RFC 3339.
///
/// Rendered here rather than downstream because everything that consumes these
/// values — the admin API, the UI, the MCP tool — wants a string, and a
/// `chrono` type threaded through the wire models would only be formatted at the
/// very end anyway.
pub fn opt_timestamp(row: &Row, name: &str) -> Option<String> {
    row.try_get::<_, Option<chrono::DateTime<chrono::Utc>>>(name)
        .ok()
        .flatten()
        .map(|v| v.to_rfc3339())
}

/// `SelectEntity` for a statistics row struct.
///
/// The trait exists to serve `my-postgres`' query builder, but the statistics
/// queries are hand-written SQL with casts and window functions that no builder
/// would produce — so only `from(row)` is ever called and the other three
/// methods are dead weight on every row struct. This spares each of them the
/// same three no-op bodies; the struct supplies `read_row` instead.
macro_rules! stats_row {
    ($t:ty) => {
        impl my_postgres::sql_select::SelectEntity for $t {
            fn from(row: &my_postgres::tokio_postgres::Row) -> Self {
                <$t>::read_row(row)
            }

            fn fill_select_fields(_select_builder: &mut my_postgres::sql::SelectBuilder) {}

            fn get_order_by_fields() -> Option<&'static str> {
                None
            }

            fn get_group_by_fields() -> Option<&'static str> {
                None
            }
        }
    };
}

pub(crate) use stats_row;
