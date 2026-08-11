//! Per-database statistics: what the server is doing, and what it costs.
//!
//! # What Postgres can and cannot tell you
//!
//! There is **no CPU figure in the catalog**. Not for a superuser, not for
//! `pg_monitor` — no system view exposes host CPU at all, and reading it needs an
//! agent on the machine or an extension like `pg_proctab`, neither of which this
//! server has. Everything here is therefore a *proxy*, and each one says in its
//! own docs how far it can be trusted:
//!
//! - [`DbHealthRates::busy_backends`] — Δ`pg_stat_database.active_time` over
//!   wall-clock, i.e. how many backends were executing on average. The headline
//!   number, and the closest honest answer to "is the database busy".
//! - [`TopStatement::exec_ms_per_sec`] — the same idea per normalized statement,
//!   from `pg_stat_statements`. This is what answers "*which* query is burning
//!   the server".
//! - [`ActivityStats`] — who is connected and what is blocked, from
//!   `pg_stat_activity`.
//!
//! Sizes, by contrast, are exact and need no privileges at all: see
//! [`TablesStats`].
//!
//! # Why an admin account matters
//!
//! Only for the parts that read *other users'* rows. Postgres returns a row per
//! backend and per statement to everyone, but blanks `state`, `query` and
//! `wait_event` for backends the account does not own unless it is a member of
//! `pg_monitor`/`pg_read_all_stats`. So an ordinary account gets correct sizes,
//! correct database-wide counters, and an undercounted, textless view of activity.
//! That is reported rather than hidden — [`ServerCapabilities::can_read_all_stats`]
//! travels all the way to the UI, and the counts it undermines are published next
//! to [`ActivityStats::state_unknown`].
//!
//! # Shape
//!
//! Collection runs on two timers ([`collector`]) and publishes into a per-database
//! cache ([`DbStatsCache`]); readers — `GET /api/Stats` and the `db_stats` MCP tool
//! — only ever see the cache. Nothing here goes through [`crate::sql_guard`] or the
//! request log; see [`crate::postgres::PostgresAccess::query_typed`] for why.

mod activity;
pub use activity::*;

mod cache;
pub use cache::*;

mod capabilities;
pub use capabilities::*;

pub mod collector;

mod health;
pub use health::*;

mod longest_seen;
pub use longest_seen::*;

mod public_model;
pub use public_model::*;

mod section;
pub use section::*;

mod snapshot;
pub use snapshot::*;

mod statements;
pub use statements::*;

mod store;
pub use store::*;

mod tables;
pub use tables::*;
