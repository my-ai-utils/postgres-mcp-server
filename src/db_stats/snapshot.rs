use rust_extensions::date_time::DateTimeAsMicroseconds;

use super::{
    ActivityStats, DbHealth, DiskIo, MinuteThroughput, Section, ServerCapabilities, TablesStats,
    TopStatements,
};

/// Everything the collector knows about one database, as of the last tick.
///
/// Published as a whole `Arc` so a reader — the admin API or an MCP tool call —
/// takes a consistent set of sections without holding the lock across
/// serialization, and so a section that failed this tick keeps the value it had
/// rather than blanking the card.
#[derive(Debug, Clone, Default)]
pub struct DbStatsSnapshot {
    /// When the fast sections were last refreshed. `None` before the first tick.
    pub collected_at: Option<DateTimeAsMicroseconds>,
    /// When the slow sections — capabilities, tables, statements — were last
    /// refreshed. They move on a much longer timer, so a single "collected"
    /// timestamp would misdate them by up to a minute.
    pub slow_collected_at: Option<DateTimeAsMicroseconds>,
    /// Set when the connection itself is the problem, rather than one section.
    ///
    /// Every section fails on its own when the query fails, but a database that is
    /// simply unreachable fails all of them with the same driver error repeated
    /// four times. Hoisting it here lets the UI say "this database is down" once.
    pub last_error: Option<String>,
    pub server: Section<ServerCapabilities>,
    pub activity: Section<ActivityStats>,
    pub health: Section<DbHealth>,
    pub tables: Section<TablesStats>,
    pub statements: Section<TopStatements>,
    pub disk_io: Section<DiskIo>,
    /// The last completed minute of traffic. `None` until two slow ticks have run.
    pub throughput: Option<MinuteThroughput>,
}
