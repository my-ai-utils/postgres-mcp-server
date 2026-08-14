use std::sync::Arc;
use std::time::Duration;

use rust_extensions::date_time::DateTimeAsMicroseconds;
use rust_extensions::{MyTimerTick, RepeatTimerIteration};

use crate::app::{AppContext, DbContext};

use super::{FastTick, LoadSample, Section, ServerCapabilities, SlowTick};

/// How often the cheap sections are refreshed, and the resolution of the stored
/// load series. Connection counts and the busy-backends figure are only
/// interesting if they are roughly live, and both queries are a single aggregate
/// over an in-memory view.
pub const FAST_INTERVAL: Duration = Duration::from_secs(5);

/// How often the expensive sections are refreshed in memory.
/// `pg_total_relation_size` over every table in the database is not something to
/// run every 5 seconds, and a table does not change size meaningfully faster than
/// this.
pub const SLOW_INTERVAL: Duration = Duration::from_secs(60);

/// How often table sizes and statement costs are appended to history, and how
/// often expired rows are swept.
///
/// Hourly rather than every slow tick: a table's size over three days is a growth
/// curve, and sampling it 60 times an hour would store the same number 60 times
/// while repeating every statement's SQL text alongside it. Sweeping on the same
/// timer means retention can overshoot by up to an hour, which on a three-day
/// horizon is under 1.5% and buys one timer instead of two.
pub const HOURLY_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Deliberately shorter than the 10 seconds [`crate::postgres::PostgresAccess::do_request`]
/// gives the agent's own SQL: a wedged database should cost one collection tick,
/// not stall the timer.
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);

/// Every timer iteration gets a ceiling of its own, as a backstop behind the
/// per-query bound in [`crate::postgres::PostgresAccess::query_typed`].
///
/// Databases are collected concurrently, so this bounds the slowest single
/// database rather than their sum. One database's tick makes at most five
/// sequential queries — capabilities, then the three `pg_stat_activity` reads and
/// the health counters on the fast tick, or capabilities, tables, statements and
/// the two I/O reads on the slow one — each hard-bounded at twice
/// [`QUERY_TIMEOUT`], so 50 seconds is the real worst case, and an unreachable
/// database costs only the first of them, because the tick gives up once
/// capabilities fail. The margin is deliberate: this timer firing means something is
/// wrong that the query bound did not catch, and it should not be reachable in
/// ordinary operation.
const ITERATION_TIMEOUT: Duration = Duration::from_secs(60);

/// What a section says when the database itself could not be reached. The driver
/// error is reported once, in [`super::DbStatsSnapshot::last_error`], instead of
/// being repeated in all four sections.
const UNREACHABLE: &str = "Not collected — the database could not be reached on this tick.";

pub struct FastStatsTimer {
    app: Arc<AppContext>,
}

impl FastStatsTimer {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

/// All three timers here answer [`RepeatTimerIteration::WithInterval`]: each tick
/// is one complete pass over every database, bounded by the per-query ceiling in
/// [`crate::postgres::PostgresAccess::query_typed`], and there is never a
/// half-finished batch to carry into another iteration.
///
/// `Immediately` exists for ticks that split a long job into portions to keep
/// inside `iteration_timeout`. It would be actively wrong here: the fast tick's
/// cost is dominated by *waiting* on a database, so re-entering at once would
/// mean spinning on an unreachable one instead of honouring the 5-second interval.
#[async_trait::async_trait]
impl MyTimerTick for FastStatsTimer {
    async fn tick(&self) -> RepeatTimerIteration {
        let mut running = tokio::task::JoinSet::new();

        for db in &self.app.databases {
            running.spawn(collect_fast(db.clone()));
        }

        let mut samples = Vec::with_capacity(self.app.databases.len());

        while let Some(finished) = running.join_next().await {
            // A panic inside one database's collection must not cost the others
            // their sample; JoinSet reports it as an Err here.
            if let Ok(Some(sample)) = finished {
                samples.push(sample);
            }
        }

        if samples.is_empty() {
            return RepeatTimerIteration::WithInterval;
        }

        // One commit for the whole tick — see `MetricsStore`'s module docs.
        let written = self
            .app
            .metrics
            .write_load(DateTimeAsMicroseconds::now(), samples)
            .await;

        self.app.metrics_write_error.set(written.err());

        RepeatTimerIteration::WithInterval
    }
}

pub struct SlowStatsTimer {
    app: Arc<AppContext>,
}

impl SlowStatsTimer {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

#[async_trait::async_trait]
impl MyTimerTick for SlowStatsTimer {
    async fn tick(&self) -> RepeatTimerIteration {
        let mut running = tokio::task::JoinSet::new();

        for db in &self.app.databases {
            running.spawn(collect_slow(db.clone()));
        }

        let mut minutes = Vec::with_capacity(self.app.databases.len());

        while let Some(finished) = running.join_next().await {
            // A panic in one database's collection must not cost the others their
            // minute; JoinSet reports it as an Err here.
            if let Ok(Some(minute)) = finished {
                minutes.push(minute);
            }
        }

        if !minutes.is_empty() {
            // The slow tick *is* the minute, so the row is written here rather than on
            // the hourly timer — per-minute resolution is the whole point of it.
            let written = self
                .app
                .metrics
                .write_minutes(DateTimeAsMicroseconds::now(), minutes)
                .await;

            if let Some(err) = written.err() {
                self.app.metrics_write_error.set(Some(err));
            }
        }

        RepeatTimerIteration::WithInterval
    }
}

/// Appends the hourly history rows, then deletes everything past the retention
/// horizon.
///
/// Reads the caches the slow timer has already filled rather than querying
/// Postgres again — the numbers are at most a minute old, which is far inside an
/// hourly sample, and it keeps history writing off the database entirely.
pub struct HourlyHistoryTimer {
    app: Arc<AppContext>,
}

impl HourlyHistoryTimer {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

#[async_trait::async_trait]
impl MyTimerTick for HourlyHistoryTimer {
    async fn tick(&self) -> RepeatTimerIteration {
        if !self.app.metrics.is_enabled() {
            // Still drain the hour, even with nowhere to write it: leaving it filled
            // would turn "the longest queries this hour" into "the longest ever
            // seen", so if history is enabled later the first hour recorded would be
            // a lie.
            for db in &self.app.databases {
                db.longest_seen.take_top();
            }

            return RepeatTimerIteration::WithInterval;
        }

        let at = DateTimeAsMicroseconds::now();

        let mut error = None;

        for db in &self.app.databases {
            let snapshot = db.stats.get();

            // Draining the accumulator also starts the next hour, so this must run
            // exactly once per hour per database even if the write below fails —
            // otherwise a single very slow query would head every hour after it.
            let longest = db.longest_seen.take_top();

            if let Err(err) = self
                .app
                .metrics
                .write_hourly(
                    at,
                    &snapshot.tables,
                    &snapshot.statements,
                    longest,
                    db.path.as_str(),
                )
                .await
            {
                error = Some(err);
            }
        }

        match self.app.metrics.collect_garbage().await {
            Ok(collected) => {
                if collected.total() > 0 {
                    println!(
                        "Metrics history: deleted {} expired rows (load {}, table sizes {}, \
                         statements {}, longest {})",
                        collected.total(),
                        collected.load,
                        collected.table_sizes,
                        collected.statements,
                        collected.longest
                    );
                }
            }
            Err(err) => error = Some(err),
        }

        if error.is_some() {
            self.app.metrics_write_error.set(error);
        }

        RepeatTimerIteration::WithInterval
    }
}

/// Capabilities, from the cache if a tick has already read them.
///
/// On the first fast tick after start-up there is nothing cached yet, and waiting
/// for the slow timer would leave the page empty for up to a minute — so the fast
/// tick reads them itself, once.
async fn capabilities_for_fast_tick(db: &Arc<DbContext>) -> Result<ServerCapabilities, String> {
    if let Some(capabilities) = db.stats.capabilities() {
        return Ok(capabilities);
    }

    let capabilities = super::collect_capabilities(&db.postgres, QUERY_TIMEOUT).await;

    db.stats
        .apply_capabilities(Section::from_result(capabilities.clone()));

    capabilities
}

/// Refreshes one database's live sections and returns its history row, if the tick
/// produced anything worth storing.
async fn collect_fast(db: Arc<DbContext>) -> Option<(String, LoadSample)> {
    let capabilities = match capabilities_for_fast_tick(&db).await {
        Ok(capabilities) => capabilities,
        Err(err) => {
            // Nothing else can be built without knowing the server version, and
            // this particular failure means the connection is down rather than one
            // view being off-limits.
            db.stats.apply_fast_tick(FastTick {
                activity: Section::Unavailable(UNREACHABLE.to_string()),
                health: Section::Unavailable(UNREACHABLE.to_string()),
                last_error: Some(err),
            });

            // Deliberately no row: a gap in the series is the truthful record of a
            // database that was unreachable, whereas a row of nulls would plot as
            // "zero load".
            return None;
        }
    };

    let activity =
        super::collect_activity(&db.postgres, capabilities.server_version_num, QUERY_TIMEOUT).await;

    let health = super::collect_health(&db.postgres, &capabilities, QUERY_TIMEOUT).await;

    // Every tick contributes to the hour's longest-query ranking. This has to happen
    // here, on the 5-second timer, because `pg_stat_activity` shows only what is
    // executing at this instant: an hourly read would catch just the statements
    // that happened to be running in that one second.
    if let Ok(activity) = &activity {
        db.longest_seen.observe(activity.longest.as_slice());
    }

    db.stats.apply_fast_tick(FastTick {
        activity: Section::from_result(activity),
        health: Section::from_result(health),
        last_error: None,
    });

    // Read back what was published, so the stored row is exactly what the UI shows
    // — including the rates, which only exist once the cache has diffed this sample
    // against the previous one.
    let snapshot = db.stats.get();

    LoadSample::new(&snapshot.health, &snapshot.activity)
        .map(|sample| (db.path.as_str().to_string(), sample))
}

async fn collect_slow(db: Arc<DbContext>) -> Option<(String, super::MinuteThroughput)> {
    let capabilities = super::collect_capabilities(&db.postgres, QUERY_TIMEOUT).await;

    let capabilities = match capabilities {
        Ok(capabilities) => capabilities,
        Err(err) => {
            db.stats.apply_slow_tick(SlowTick {
                server: Section::Unavailable(err),
                tables: Section::Unavailable(UNREACHABLE.to_string()),
                statements: Section::Unavailable(UNREACHABLE.to_string()),
                disk_io: Section::Unavailable(UNREACHABLE.to_string()),
                throughput: None,
            });
            return None;
        }
    };

    let tables = super::collect_tables(&db.postgres, QUERY_TIMEOUT).await;

    let previous_statements = db.stats.previous_statements();

    let statements = super::collect_statements(
        &db.postgres,
        &capabilities,
        previous_statements.as_ref(),
        QUERY_TIMEOUT,
    )
    .await;

    // The minute's throughput is assembled here rather than inside either source,
    // because it needs both: the counts and the average come from the extension's
    // deltas, while the longest comes from the 5-second sampler — see
    // `super::throughput`. Draining the sampler starts the next minute, so it happens
    // exactly once per slow tick whatever the statements query did.
    let longest_this_minute = db.longest_seen_minute.take_top();

    let throughput = statements.as_ref().ok().map(|(_, _, totals)| {
        super::MinuteThroughput::new(
            totals.window_secs.unwrap_or_default(),
            totals.calls,
            totals.total_exec_ms,
            longest_this_minute.first(),
            totals.record.clone(),
        )
    });

    let statements = statements.map(|(top, snapshot, _)| (top, snapshot));

    let disk_io = super::collect_disk_io(
        &db.postgres,
        &capabilities,
        db.stats.previous_disk_io().as_ref(),
        QUERY_TIMEOUT,
    )
    .await;

    db.stats.apply_slow_tick(SlowTick {
        server: Section::Ready(capabilities),
        tables: Section::from_result(tables),
        statements: Section::from_result(statements),
        disk_io: Section::from_result(disk_io),
        throughput: throughput.clone(),
    });

    // Handed back rather than written here: the store lives on the AppContext, and
    // one commit for every database beats one per mount — see `MetricsStore`.
    throughput.map(|throughput| (db.path.as_str().to_string(), throughput))
}

/// Starts the three collection timers.
///
/// `set_first_tick_before_delay` matters more than it looks: without it the admin
/// page would show four "pending" cards for the first 60 seconds after a restart,
/// which is indistinguishable from the collector being broken — and the retention
/// sweep would not run until an hour in.
pub fn start(app: &Arc<AppContext>) {
    start_timer(
        app,
        "DbStatsFast",
        FAST_INTERVAL,
        Arc::new(FastStatsTimer::new(app.clone())),
    );

    start_timer(
        app,
        "DbStatsSlow",
        SLOW_INTERVAL,
        Arc::new(SlowStatsTimer::new(app.clone())),
    );

    start_timer(
        app,
        "DbStatsHourlyHistory",
        HOURLY_INTERVAL,
        Arc::new(HourlyHistoryTimer::new(app.clone())),
    );
}

fn start_timer(
    app: &Arc<AppContext>,
    name: &str,
    interval: Duration,
    tick: Arc<dyn MyTimerTick + Send + Sync + 'static>,
) {
    let mut timer = rust_extensions::MyTimer::new_with_execute_timeout(interval, ITERATION_TIMEOUT);
    timer.set_first_tick_before_delay();
    timer.register_timer(name, tick);
    timer.start(app.app_states.clone(), my_logger::LOGGER.clone());
}
