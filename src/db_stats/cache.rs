use std::sync::Arc;

use parking_lot::Mutex;
use rust_extensions::date_time::DateTimeAsMicroseconds;

use super::{
    ActivityStats, DbHealth, DbHealthSample, DbStatsSnapshot, Section, ServerCapabilities,
    StatementsSnapshot, TablesStats, TopStatements,
};

/// What the last fast tick produced.
pub struct FastTick {
    pub activity: Section<ActivityStats>,
    pub health: Section<DbHealthSample>,
    pub last_error: Option<String>,
}

/// What the last slow tick produced. `statements` carries the snapshot the *next*
/// slow tick diffs against, so the cache is the only owner of it.
pub struct SlowTick {
    pub server: Section<ServerCapabilities>,
    pub tables: Section<TablesStats>,
    pub statements: Section<(TopStatements, StatementsSnapshot)>,
}

/// One database's published statistics, plus the raw previous samples the rate
/// calculations need.
///
/// The previous samples live here rather than in the collector because this is the
/// only place that holds both the old and the new reading at the same instant —
/// putting the subtraction anywhere else would mean handing the previous sample
/// out and trusting the caller to hand a new one back.
///
/// Written twice a minute at most and read on every UI poll, so this is genuinely
/// read-mostly; the lock is nevertheless a plain `Mutex` and is only ever held
/// long enough to swap an `Arc`, never across serialization or an `.await`.
pub struct DbStatsCache {
    state: Mutex<CacheState>,
}

struct CacheState {
    snapshot: Arc<DbStatsSnapshot>,
    previous_health: Option<DbHealthSample>,
    previous_statements: Option<StatementsSnapshot>,
}

impl DbStatsCache {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(CacheState {
                snapshot: Arc::new(DbStatsSnapshot::default()),
                previous_health: None,
                previous_statements: None,
            }),
        }
    }

    pub fn get(&self) -> Arc<DbStatsSnapshot> {
        self.state.lock().snapshot.clone()
    }

    /// The capabilities the last slow tick read, which every version-gated query
    /// needs before it can be built. `None` until the first slow tick lands — the
    /// collector then skips the gated sections for one round rather than guessing
    /// a version.
    pub fn capabilities(&self) -> Option<ServerCapabilities> {
        self.state
            .lock()
            .snapshot
            .server
            .data()
            .cloned()
    }

    /// Previous statement counters, for the next diff. Cloned out so the lock is
    /// released before the collector goes near the network.
    pub fn previous_statements(&self) -> Option<StatementsSnapshot> {
        self.state.lock().previous_statements.clone()
    }

    /// Capabilities on their own.
    ///
    /// Both timers write this field: the slow one re-reads it every minute (an
    /// extension can be installed, a role granted, while the server runs), and the
    /// fast one fills it in on the very first tick so it does not have to sit out
    /// a whole minute waiting for the slow timer to tell it the server version.
    /// Both write the same truth, so last-writer-wins is the correct resolution.
    pub fn apply_capabilities(&self, server: Section<ServerCapabilities>) {
        let mut state = self.state.lock();

        let mut snapshot = (*state.snapshot).clone();
        snapshot.server = server;

        state.snapshot = Arc::new(snapshot);
    }

    pub fn apply_fast_tick(&self, tick: FastTick) {
        let mut state = self.state.lock();

        // Only a successful read replaces the retained sample. Rebasing the rates
        // on a failed tick would silently widen the next window; keeping the last
        // good sample means the tick after a blip reports a longer window with
        // correct arithmetic.
        let health = match tick.health {
            Section::Ready(sample) => {
                let health = DbHealth::new(&sample, state.previous_health.as_ref());
                state.previous_health = Some(sample);
                Section::Ready(health)
            }
            Section::Unavailable(reason) => Section::Unavailable(reason),
            Section::Pending => Section::Pending,
        };

        let mut snapshot = (*state.snapshot).clone();
        snapshot.collected_at = Some(DateTimeAsMicroseconds::now());
        snapshot.last_error = tick.last_error;
        snapshot.activity = tick.activity;
        snapshot.health = health;

        state.snapshot = Arc::new(snapshot);
    }

    pub fn apply_slow_tick(&self, tick: SlowTick) {
        let mut state = self.state.lock();

        let statements = match tick.statements {
            Section::Ready((top, statements_snapshot)) => {
                state.previous_statements = Some(statements_snapshot);
                Section::Ready(top)
            }
            Section::Unavailable(reason) => Section::Unavailable(reason),
            Section::Pending => Section::Pending,
        };

        let mut snapshot = (*state.snapshot).clone();
        snapshot.slow_collected_at = Some(DateTimeAsMicroseconds::now());
        snapshot.server = tick.server;
        snapshot.tables = tick.tables;
        snapshot.statements = statements;

        state.snapshot = Arc::new(snapshot);
    }
}
