use std::time::Duration;

use dioxus::prelude::*;

use crate::components::atoms::{StatePill, StateTone};
use crate::components::{
    ChartPoint, LoadCharts, LoadSeries, MinuteCharts, MinuteSeries, MinuteUnit, Topbar,
};
use crate::models::{
    Activity, DatabaseStats, DiskIo, Health, HistoryInfo, Load, ServerInfo, ServerRef,
    ServerSettings, ServerStats, Tables, Throughput, WriteIo, fmt, section,
};

/// Slower than the requests page's 1s: nothing here moves faster than the
/// collector's 5-second tick, so polling more often would only spend battery
/// re-rendering identical numbers.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// The chart window — the last hour.
const CHART_HOURS: i64 = 1;

/// The panels are redrawn on their own, slower clock: an hour of 5-second samples is
/// ~720 points per database, and at the page's cadence it would be re-fetched twelve
/// times before a single new point could change the shape.
const CHART_REFRESH: Duration = Duration::from_secs(30);

#[derive(Default)]
pub struct StatsState {
    stats: ServerStats,
    /// Only for the topbar's write-access pill. Fetched here rather than passed in
    /// so the pill says the same thing on every page — a topbar that claimed
    /// "writes off" just because this page does not track them would be worse than
    /// one extra small request per tick.
    settings: ServerSettings,
    /// Which server is on screen. `None` until the first poll names one.
    ///
    /// Kept as the key rather than an index: the settings file can gain a database
    /// while the page is open, and an index would silently point at a different
    /// server when the list shifts.
    selected_server: Option<String>,
    /// One entry per database of the selected server — never summed; see
    /// [`LoadCharts`] for why.
    chart: Vec<LoadSeries>,
    /// Which server `chart` belongs to, so a switch mid-fetch cannot leave one
    /// server's panels under another's heading.
    chart_server: Option<String>,
    chart_from_ms: i64,
    chart_to_ms: i64,
    /// Why there is nothing to draw at all. Starts as "loading" rather than "no
    /// data": the first fetch takes a moment, and an empty card claiming there is
    /// nothing recorded would be wrong for that moment.
    chart_note: String,
    /// The per-minute series of each database, keyed by mount path. Fetched
    /// alongside the load panels so both share one refresh clock.
    minutes: Vec<(String, Vec<MinuteSeries>)>,
    /// Window the minute series covers.
    minutes_from_ms: i64,
    minutes_to_ms: i64,
    /// Mount whose extension install is in flight.
    extension_busy: Option<String>,
    /// What the last install attempt reported, verbatim.
    extension_message: Option<String>,
    /// Mount whose track_io_timing change is in flight, so only that card's buttons
    /// go quiet.
    io_timing_busy: Option<String>,
    /// Why the last attempt failed — Postgres' own message.
    io_timing_error: Option<String>,
    /// Why the last poll failed, if it did. Kept alongside the previous values
    /// rather than replacing them — a page that blanks on one dropped request is
    /// worse than a page that says "this is a few seconds stale".
    error: Option<String>,
}

impl StatsState {
    /// A fresh page has not fetched anything yet — say so, rather than claiming the
    /// database has recorded nothing.
    fn new() -> Self {
        Self {
            chart_note: "Loading…".to_string(),
            ..Default::default()
        }
    }

    /// The server whose section is shown, falling back to the first configured one.
    fn current_server(&self) -> Option<ServerRef> {
        let servers = self.stats.servers();

        if let Some(selected) = self.selected_server.as_deref() {
            if let Some(found) = servers.iter().find(|server| server.key == selected) {
                return Some(found.clone());
            }
        }

        servers.into_iter().next()
    }
}

async fn poll_once(mut cs: Signal<StatsState>) {
    let stats = match crate::api::get_server_stats().await {
        Ok(stats) => Some(stats),
        Err(err) => {
            dioxus_utils::console_log(format!("Stats poll failed: {}", err));
            cs.write().error = Some(err.to_string());
            None
        }
    };

    let settings = match crate::api::get_server_settings().await {
        Ok(settings) => Some(settings),
        Err(err) => {
            dioxus_utils::console_log(format!("Settings poll failed: {}", err));
            None
        }
    };

    let mut write = cs.write();

    if let Some(stats) = stats {
        write.stats = stats;
        write.error = None;
    }

    if let Some(settings) = settings {
        write.settings = settings;
    }
}

/// Rebuilds the panels for whichever server is selected — **one per database, never
/// summed**.
///
/// Two mounts can open the same database, in which case they read the very same
/// `pg_stat_database` counters. Their series are identical, so the second is marked
/// as a copy instead of being drawn as if it were more load.
async fn refresh_chart(mut cs: Signal<StatsState>) {
    let Some(server) = cs.peek().current_server() else {
        // No server yet means the first stats poll has not landed. Leave the note as
        // it is — the poller will call back here.
        return;
    };

    // Captured before the awaits below: the operator can switch servers while these
    // requests are in flight, and applying a stale result would put one server's
    // panels under another's heading.
    let requested = server.key.clone();

    let databases = cs.peek().stats.databases_on(requested.as_str());

    let mut series: Vec<LoadSeries> = Vec::with_capacity(databases.len());
    let mut minutes: Vec<(String, Vec<MinuteSeries>)> = Vec::with_capacity(databases.len());
    let mut minutes_to_ms = 0_i64;
    let mut seen_databases: Vec<(String, String)> = Vec::new();

    for db in &databases {
        // Same database as an earlier mount? Then this line is that line.
        let duplicate_of = db.server.database_key.as_ref().and_then(|key| {
            seen_databases
                .iter()
                .find(|(seen, _)| seen == key)
                .map(|(_, path)| path.clone())
        });

        if let Some(key) = db.server.database_key.clone() {
            seen_databases.push((key, db.path.clone()));
        }

        let (points, note) = match crate::api::get_load_history(db.path.as_str(), CHART_HOURS).await
        {
            Ok(history) => {
                let points: Vec<ChartPoint> = history
                    .load
                    .iter()
                    // A sample with no busy figure — Postgres older than 14, or the
                    // first tick after a stats reset — is left out rather than
                    // plotted as zero, which would read as "idle".
                    .filter_map(|point| {
                        point.busy_backends.map(|value| ChartPoint {
                            at_unix_ms: point.at_unix_ms,
                            value,
                        })
                    })
                    .collect();

                let note = history.error.or_else(|| {
                    if points.is_empty() {
                        Some(
                            "Nothing recorded yet — this needs Postgres 14+, and the first \
                             figure appears one tick after start-up."
                                .to_string(),
                        )
                    } else {
                        None
                    }
                });

                (points, note)
            }
            Err(err) => {
                dioxus_utils::console_log(format!("Load history for {} failed: {}", db.path, err));
                (Vec::new(), Some(err.to_string()))
            }
        };

        series.push(LoadSeries {
            description: db.description.clone(),
            path: db.path.clone(),
            points,
            note,
            duplicate_of,
        });

        // The three per-minute measures. Separate request from the load series: a
        // different section, a different resolution, and only this card reads it.
        if let Ok(history) = crate::api::get_minute_history(db.path.as_str(), CHART_HOURS).await {
            if let Some(last) = history.minutes.last() {
                minutes_to_ms = minutes_to_ms.max(last.at_unix_ms);
            }

            minutes.push((db.path.clone(), minute_series(&history.minutes)));
        }
    }

    // The window is anchored to the newest sample across the panels rather than to
    // the browser's clock: the two can differ by minutes, and an axis ending in the
    // future would squeeze a live chart into the left of the card.
    let to_ms = series
        .iter()
        .filter_map(|s| s.points.last().map(|point| point.at_unix_ms))
        .max()
        .unwrap_or(0);

    let note = if series.is_empty() {
        "No database is configured on this server.".to_string()
    } else {
        series
            .iter()
            .find_map(|s| s.note.clone())
            .unwrap_or_else(|| "Nothing recorded yet.".to_string())
    };

    let mut write = cs.write();

    // Dropped on the floor if the operator switched servers while this was in
    // flight; the switch schedules its own refresh.
    if write.current_server().map(|server| server.key).as_deref() != Some(requested.as_str()) {
        return;
    }

    write.chart = series;
    write.minutes = minutes;
    write.minutes_to_ms = minutes_to_ms;
    write.minutes_from_ms = minutes_to_ms - CHART_HOURS * 60 * 60 * 1_000;
    write.chart_server = Some(requested);
    write.chart_from_ms = to_ms - CHART_HOURS * 60 * 60 * 1_000;
    write.chart_to_ms = to_ms;
    write.chart_note = note;
}

/// The three measures, each on its own panel because each is in its own unit — see
/// [`MinuteCharts`] for why they must not share an axis.
///
/// A row with no value for a measure contributes no point to that panel rather than a
/// zero: a minute in which nothing ran long enough to be sampled did not have a
/// longest query of zero seconds.
fn minute_series(rows: &[crate::models::MinutePoint]) -> Vec<MinuteSeries> {
    let point = |at_unix_ms: i64, value: f64| ChartPoint { at_unix_ms, value };

    vec![
        MinuteSeries {
            title: "Queries".to_string(),
            note: "per minute".to_string(),
            unit: MinuteUnit::Count,
            points: rows
                .iter()
                .filter_map(|row| row.calls.map(|calls| point(row.at_unix_ms, calls as f64)))
                .collect(),
        },
        MinuteSeries {
            title: "Average time".to_string(),
            note: "per query".to_string(),
            unit: MinuteUnit::Milliseconds,
            points: rows
                .iter()
                .filter_map(|row| row.avg_exec_ms.map(|ms| point(row.at_unix_ms, ms)))
                .collect(),
        },
        MinuteSeries {
            title: "Longest".to_string(),
            note: "sampled — a floor, not a maximum".to_string(),
            unit: MinuteUnit::Seconds,
            points: rows
                .iter()
                .filter_map(|row| row.longest_secs.map(|secs| point(row.at_unix_ms, secs)))
                .collect(),
        },
    ]
}

/// Tone for a fill ratio: fine until it is most of the way there.
fn saturation_tone(ratio: Option<f64>) -> StateTone {
    match ratio {
        Some(ratio) if ratio >= 0.9 => StateTone::Bad,
        Some(ratio) if ratio >= 0.7 => StateTone::Warn,
        Some(_) => StateTone::Ok,
        None => StateTone::Neutral,
    }
}

/// Busy backends is not a percentage, so it has no natural ceiling — but one
/// backend continuously executing is the point where "the database is working" turns
/// into "the database is the bottleneck".
fn busy_tone(busy: Option<f64>) -> StateTone {
    match busy {
        Some(busy) if busy >= 2.0 => StateTone::Bad,
        Some(busy) if busy >= 1.0 => StateTone::Warn,
        Some(_) => StateTone::Ok,
        None => StateTone::Neutral,
    }
}

/// Milliseconds of waiting per wall-clock second. 1000 is a whole backend doing
/// nothing but waiting for the disk, so the bands sit well below it — by the time a
/// database loses a full second per second, it has been in trouble for a while.
fn io_wait_tone(ms_per_sec: Option<f64>) -> StateTone {
    match ms_per_sec {
        Some(ms) if ms >= 500.0 => StateTone::Bad,
        Some(ms) if ms >= 100.0 => StateTone::Warn,
        Some(_) => StateTone::Ok,
        None => StateTone::Neutral,
    }
}

/// What share of "executing" was really waiting. Half is the point where the
/// database is more storage-bound than work-bound.
fn io_share_tone(share: Option<f64>) -> StateTone {
    match share {
        Some(share) if share >= 0.5 => StateTone::Bad,
        Some(share) if share >= 0.25 => StateTone::Warn,
        Some(_) => StateTone::Ok,
        None => StateTone::Neutral,
    }
}

/// A cache that has to go to disk for more than a few percent of reads is worth
/// noticing; the scale is inverted, hence its own function.
fn cache_tone(ratio: Option<f64>) -> StateTone {
    match ratio {
        Some(ratio) if ratio < 0.9 => StateTone::Bad,
        Some(ratio) if ratio < 0.98 => StateTone::Warn,
        Some(_) => StateTone::Ok,
        None => StateTone::Neutral,
    }
}

/// One headline number.
#[component]
fn StatTile(
    label: String,
    value: String,
    #[props(default = String::new())] hint: String,
    #[props(default = StateTone::Neutral)] tone: StateTone,
    #[props(default = None)] title: Option<String>,
) -> Element {
    let tone_class = match tone {
        StateTone::Ok => "tile--ok",
        StateTone::Warn => "tile--warn",
        StateTone::Bad => "tile--bad",
        StateTone::Neutral => "tile--neutral",
    };

    rsx! {
        div { class: "tile {tone_class}", title,
            span { class: "tile__label", "{label}" }
            span { class: "tile__value mono", "{value}" }
            if !hint.is_empty() {
                span { class: "tile__hint", "{hint}" }
            }
        }
    }
}

/// Why a section has nothing to show — the collector has not run yet, or this
/// server/account cannot produce it. Never rendered as an empty card, because
/// "empty" and "not allowed" call for different reactions.
#[component]
fn SectionNotice(state: String, reason: Option<String>) -> Element {
    let (tone, text) = match state.as_str() {
        section::PENDING => (
            StateTone::Neutral,
            "Not collected yet — the first tick is on its way.".to_string(),
        ),
        section::UNAVAILABLE => (
            StateTone::Warn,
            reason.unwrap_or_else(|| "Not available on this server.".to_string()),
        ),
        _ => (StateTone::Neutral, "No data.".to_string()),
    };

    rsx! {
        div { class: "notice",
            StatePill { label: String::new(), tone }
            span { "{text}" }
        }
    }
}

/// The identity of the connection, and what it is allowed to see.
///
/// The privilege badge is not decoration: without `pg_monitor` the activity counts
/// undercount and the query texts are missing, so the page has to say so next to
/// the numbers it affects rather than leaving them looking authoritative.
#[component]
fn ServerBadges(server: ServerInfo) -> Element {
    if !server.is_collected() {
        return rsx! {
            SectionNotice { state: server.state.clone(), reason: server.reason.clone() }
        };
    }

    let superuser = server.is_superuser.unwrap_or(false);
    let sees_all = server.sees_all_stats();
    let has_extension = server.has_pg_stat_statements.unwrap_or(false);

    rsx! {
        div { class: "badges",
            span { class: "badges__summary mono", "{server.summary()}" }

            if superuser {
                StatePill { label: "superuser".to_string(), tone: StateTone::Ok }
            } else if sees_all {
                StatePill { label: "pg_monitor".to_string(), tone: StateTone::Ok }
            } else {
                StatePill {
                    label: "limited stats".to_string(),
                    tone: StateTone::Warn,
                    title: Some(
                        "This account is not a member of pg_monitor or pg_read_all_stats, so Postgres blanks other users' rows: the connection counts below are undercounts and their query texts are missing."
                            .to_string(),
                    ),
                }
            }

            if has_extension {
                StatePill { label: "pg_stat_statements".to_string(), tone: StateTone::Ok }
            } else {
                StatePill {
                    label: "no pg_stat_statements".to_string(),
                    tone: StateTone::Warn,
                    title: Some(
                        "The extension is not installed, so per-statement execution time cannot be read."
                            .to_string(),
                    ),
                }
            }
        }
    }
}

#[component]
fn LoadTiles(health: Health, activity: Activity) -> Element {
    let busy = health.busy_backends();
    let cache = health.cache_hit_ratio();

    let cache_hint = if health.cache_hit_is_windowed() {
        "last window"
    } else {
        "since stats reset"
    };

    rsx! {
        div { class: "tiles",
            StatTile {
                label: "Busy backends".to_string(),
                value: fmt::float(busy, 2),
                hint: "executing".to_string(),
                tone: busy_tone(busy),
                title: Some(
                    "Backend-seconds of execution per wall-clock second in this database, from pg_stat_database.active_time. 1.00 means one backend was executing continuously; 3.00 means three were, on average. Not a CPU figure: a backend waiting on disk or on a lock still counts as executing, and this sees only this database. active_time is credited when a backend reports its state, so a single very long query shows up late rather than continuously. Requires Postgres 14+."
                        .to_string(),
                ),
            }
            StatTile {
                label: "Connections".to_string(),
                value: activity.connections_label(),
                hint: format!("{} here", fmt::int(activity.in_this_db)),
                tone: saturation_tone(activity.connections_ratio()),
                title: Some(
                    "Client backends across the whole cluster against max_connections — that is the pair that runs out. 'here' counts only this database."
                        .to_string(),
                ),
            }
            StatTile {
                label: "I/O wait".to_string(),
                value: match health.io_wait_ms_per_sec() {
                    Some(ms) => format!("{:.0}", ms),
                    None => fmt::NONE.to_string(),
                },
                hint: "ms per second".to_string(),
                tone: io_wait_tone(health.io_wait_ms_per_sec()),
                title: Some(
                    "Milliseconds of every wall-clock second this database spent blocked on disk reads and writes. 1000 means a full second of waiting per second — one backend doing nothing but waiting. Needs track_io_timing = on; with it off Postgres reports a hard zero, so this shows — instead."
                        .to_string(),
                ),
            }
            StatTile {
                label: "Of that, I/O".to_string(),
                value: fmt::ratio(health.io_share_of_active()),
                hint: "of execution".to_string(),
                tone: io_share_tone(health.io_share_of_active()),
                title: Some(
                    "How much of the time backends spent 'executing' was really spent waiting on the disk. High here means the busy-backends figure to the left is disk, not work."
                        .to_string(),
                ),
            }
            StatTile {
                label: "Cache hit".to_string(),
                value: fmt::ratio(cache),
                hint: cache_hint.to_string(),
                tone: cache_tone(cache),
                title: Some(
                    "Share of block reads served from shared buffers. The windowed figure is the useful one; the lifetime figure is dominated by whatever ran on the day the statistics were last reset."
                        .to_string(),
                ),
            }
            StatTile {
                label: "Commits".to_string(),
                value: fmt::float(health.commits_per_sec(), 1),
                hint: "per second".to_string(),
                tone: StateTone::Neutral,
            }
            StatTile {
                label: "Rollbacks".to_string(),
                value: fmt::float(health.rollbacks_per_sec(), 2),
                hint: "per second".to_string(),
                tone: match health.rollbacks_per_sec() {
                    Some(value) if value > 1.0 => StateTone::Warn,
                    _ => StateTone::Neutral,
                },
            }
            StatTile {
                label: "Database size".to_string(),
                value: fmt::bytes(health.db_size_bytes),
                hint: "on disk".to_string(),
                tone: StateTone::Neutral,
            }
            StatTile {
                label: "Active".to_string(),
                value: fmt::int(activity.active),
                hint: format!("{} waiting", fmt::int(activity.waiting)),
                tone: match activity.waiting {
                    Some(waiting) if waiting > 0 => StateTone::Warn,
                    _ => StateTone::Neutral,
                },
            }
            StatTile {
                label: "Idle in txn".to_string(),
                value: fmt::int(activity.idle_in_transaction),
                hint: "holds locks".to_string(),
                tone: match activity.idle_in_transaction {
                    Some(count) if count > 0 => StateTone::Warn,
                    _ => StateTone::Neutral,
                },
            }
            StatTile {
                label: "Deadlocks".to_string(),
                value: fmt::int(health.deadlocks),
                hint: "since reset".to_string(),
                tone: match health.deadlocks {
                    Some(count) if count > 0 => StateTone::Warn,
                    _ => StateTone::Neutral,
                },
            }
            StatTile {
                label: "Temp spilled".to_string(),
                value: fmt::bytes(health.temp_bytes),
                hint: format!("{} files", fmt::int(health.temp_files)),
                tone: match health.temp_bytes {
                    Some(bytes) if bytes > 0 => StateTone::Warn,
                    _ => StateTone::Neutral,
                },
                title: Some(
                    "Bytes written to temporary files because a sort or hash did not fit in work_mem."
                        .to_string(),
                ),
            }
        }
    }
}

/// Installs pg_stat_statements after asking. The confirmation spells out which of
/// the two steps this is, because only one of them needs a restart.
fn install_pg_stat_statements(mut cs: Signal<StatsState>, path: String, preload: bool) {
    let question = if preload {
        format!(
            "Add pg_stat_statements to shared_preload_libraries on the server behind {}?\n\n\
             The library is APPENDED to whatever is already listed, so other extensions \
             keep working.\n\n\
             This needs a full Postgres RESTART to take effect — a reload is not enough, \
             and this tool cannot restart it for you.",
            path
        )
    } else {
        format!(
            "Run CREATE EXTENSION IF NOT EXISTS pg_stat_statements on {}?\n\n\
             The library is already preloaded, so this takes effect immediately and needs \
             no restart. Requires privileges to create extensions.",
            path
        )
    };

    if !crate::storage::confirm(question.as_str()) {
        return;
    }

    cs.write().extension_busy = Some(path.clone());

    spawn(async move {
        let action = if preload { "preload" } else { "create" };

        let message = match crate::api::setup_pg_stat_statements(path.as_str(), action).await {
            Ok(result) => {
                let mut message = result.message;

                if let Some(error) = result.error {
                    message = format!("{} {}", message, error);
                }

                Some(message)
            }
            Err(err) => Some(err.to_string()),
        };

        {
            let mut write = cs.write();
            write.extension_busy = None;
            write.extension_message = message;
        }

        // The badge and this card both read the collected capabilities, so the page
        // only tells the truth again once the server has been re-read.
        poll_once(cs).await;
    });
}

#[component]
fn LoadCard(cs: Signal<StatsState>, path: String, load: Load, server: ServerInfo) -> Element {
    let busy = cs.read().extension_busy.as_deref() == Some(path.as_str());
    let outcome = cs.read().extension_message.clone();

    let body = if !section::is_ready(&load.state) {
        let available = server.pg_stat_statements_available.unwrap_or(false);
        let preloaded = server.pg_stat_statements_preloaded.unwrap_or(false);
        let installed = server.has_pg_stat_statements.unwrap_or(false);

        rsx! {
            div { class: "card__body",
                SectionNotice { state: load.state.clone(), reason: load.reason.clone() }

                if server.is_collected() && !available {
                    p { class: "muted", style: "margin: 10px 0 0; font-size: 12.5px;",
                        "It is not in "
                        code { class: "mono", "pg_available_extensions" }
                        " either, so the "
                        code { class: "mono", "postgresql-contrib" }
                        " package is not installed on the database host. That has to be "
                        "installed first — pointing "
                        code { class: "mono", "shared_preload_libraries" }
                        " at a library that is not on disk stops Postgres from starting."
                    }
                } else if server.is_collected() {
                    div { class: "banner banner--warn", style: "margin-top: 10px;",
                        if preloaded && !installed {
                            p { style: "margin: 0 0 8px;",
                                "The library is already preloaded — one statement finishes it, "
                                "no restart:"
                            }
                            pre { class: "banner__code mono",
                                "CREATE EXTENSION pg_stat_statements;"
                            }
                            div { class: "banner__actions",
                                button {
                                    class: "btn btn--primary btn--sm",
                                    disabled: busy,
                                    onclick: {
                                        let path = path.clone();
                                        move |_| install_pg_stat_statements(cs, path.clone(), false)
                                    },
                                    if busy { "Working…" } else { "Install it" }
                                }
                            }
                        } else {
                            p { style: "margin: 0 0 8px;",
                                "Two steps. The first needs a "
                                b { "full Postgres restart" }
                                " — it is a postmaster setting, and a reload does not pick it up:"
                            }
                            pre { class: "banner__code mono",
                                "ALTER SYSTEM SET shared_preload_libraries = '…,pg_stat_statements';\n-- restart Postgres, then:\nCREATE EXTENSION pg_stat_statements;"
                            }
                            div { class: "banner__actions",
                                button {
                                    class: "btn btn--sm",
                                    disabled: busy,
                                    onclick: {
                                        let path = path.clone();
                                        move |_| install_pg_stat_statements(cs, path.clone(), true)
                                    },
                                    if busy { "Working…" } else { "Set the preload" }
                                }
                                button {
                                    class: "btn btn--primary btn--sm",
                                    disabled: busy,
                                    onclick: {
                                        let path = path.clone();
                                        move |_| install_pg_stat_statements(cs, path.clone(), false)
                                    },
                                    "Create the extension"
                                }
                                span { class: "faint", style: "font-size: 11.5px;",
                                    "The preload is appended to what is already there, so other "
                                    "extensions keep working. Restart, then create."
                                }
                            }
                        }

                        if let Some(outcome) = outcome.clone() {
                            p { class: "banner__failure", "{outcome}" }
                        }
                    }
                }
            }
        }
    } else if load.items.is_empty() {
        rsx! {
            div { class: "card__body",
                p { class: "muted", style: "margin: 0; font-size: 12.5px;",
                    "pg_stat_statements has recorded nothing for this database yet."
                }
            }
        }
    } else {
        let rows: Vec<Element> = load
            .items
            .iter()
            .enumerate()
            .map(|(index, statement)| {
                rsx! {
                    tr { key: "{statement.query_id:?}-{index}",
                        td { class: "mono num", "{index + 1}" }
                        td {
                            span {
                                class: "mono dt-ellipsis",
                                style: "max-width: 460px;",
                                title: "{statement.query_label()}",
                                "{statement.query_label()}"
                            }
                        }
                        td { class: "mono num", "{statement.share_label()}" }
                        td { class: "mono num", "{fmt::millis(statement.mean_exec_ms)}" }
                        td { class: "mono num", "{fmt::millis(statement.total_exec_ms)}" }
                        td { class: "mono num", "{fmt::int(statement.calls)}" }
                        td { class: "mono num", "{fmt::int(statement.delta_calls)}" }
                    }
                }
            })
            .collect();

        rsx! {
            table { class: "dt",
                thead {
                    tr {
                        th { class: "num", "#" }
                        th { "Statement" }
                        th {
                            class: "num",
                            title: "Milliseconds of execution per wall-clock second since the previous minute's tick. 1000 ms/s is one backend saturated by this statement alone.",
                            "Load"
                        }
                        th { class: "num", "Mean" }
                        th { class: "num", title: "Since the extension was last reset", "Total" }
                        th { class: "num", "Calls" }
                        th { class: "num", title: "Calls since the previous tick", "New" }
                    }
                }
                tbody { {rows.into_iter()} }
            }
        }
    };

    let subtitle = match load.sees_all_statements {
        Some(false) => "this role's statements only".to_string(),
        _ => format!("top {}", load.items.len()),
    };

    rsx! {
        div { class: "card",
            div { class: "card__header",
                span { class: "card__title", "Heaviest statements" }
                span { class: "card__subtitle", "{subtitle}" }
            }
            {body}
        }
    }
}

#[component]
fn TablesCard(tables: Tables) -> Element {
    let body = if !section::is_ready(&tables.state) {
        rsx! {
            div { class: "card__body",
                SectionNotice { state: tables.state.clone(), reason: tables.reason.clone() }
            }
        }
    } else if tables.items.is_empty() {
        rsx! {
            div { class: "card__body",
                p { class: "muted", style: "margin: 0; font-size: 12.5px;",
                    "This database has no tables outside the system schemas."
                }
            }
        }
    } else {
        let rows: Vec<Element> = tables
            .items
            .iter()
            .map(|table| {
                let dead_ratio = table.dead_ratio();
                let dead_tone = match dead_ratio {
                    Some(ratio) if ratio >= 0.2 => "num state--bad-text",
                    Some(ratio) if ratio >= 0.1 => "num state--warn-text",
                    _ => "num",
                };

                rsx! {
                    tr { key: "{table.full_name()}",
                        td {
                            span { class: "mono dt-ellipsis", style: "max-width: 280px;", title: "{table.full_name()}",
                                "{table.full_name()}"
                            }
                        }
                        td { class: "mono num", "{fmt::bytes(table.total_bytes)}" }
                        td { class: "mono num", "{fmt::bytes(table.table_bytes)}" }
                        td { class: "mono num", "{fmt::bytes(table.index_bytes)}" }
                        td { class: "mono num", "{fmt::int(table.live_tuples)}" }
                        td { class: "mono {dead_tone}",
                            title: "{fmt::ratio(dead_ratio)} of estimated rows are dead",
                            "{fmt::int(table.dead_tuples)}"
                        }
                        td { class: "mono num", "{fmt::int(table.seq_scans)}" }
                        td {
                            class: if table.never_used_an_index() { "mono num state--warn-text" } else { "mono num" },
                            title: if table.never_used_an_index() {
                                "Sequentially scanned and never scanned by index — a missing index, or a table small enough not to need one."
                            } else {
                                ""
                            },
                            "{fmt::int(table.idx_scans)}"
                        }
                        td { class: "mono", "{fmt::date_time(table.last_vacuum.as_deref())}" }
                    }
                }
            })
            .collect();

        rsx! {
            table { class: "dt",
                thead {
                    tr {
                        th { "Table" }
                        th { class: "num", title: "Heap + indexes + TOAST", "Total" }
                        th { class: "num", title: "Heap + TOAST, without indexes", "Data" }
                        th { class: "num", "Indexes" }
                        th { class: "num", title: "Planner estimate, not count(*)", "Live rows" }
                        th {
                            class: "num",
                            title: "Row versions left behind by UPDATE and DELETE. Postgres does not overwrite a row — it writes a new version and marks the old one dead — so these keep occupying space, and every scan reads past them, until VACUUM reclaims it.",
                            "Dead rows"
                        }
                        th { class: "num", "Seq scans" }
                        th { class: "num", "Idx scans" }
                        th { "Last vacuum" }
                    }
                }
                tbody { {rows.into_iter()} }
            }
        }
    };

    rsx! {
        div { class: "card",
            div { class: "card__header",
                span { class: "card__title", "Largest tables" }
                span { class: "card__subtitle", "{tables.subtitle()}" }
            }
            {body}
        }
    }
}

/// Where the disk time goes: which tables are read off disk, and how much is being
/// written.
///
/// The read half is per database; the write half (WAL, checkpoints) is a property of
/// the whole server, and is labelled as such so nobody reads it as this database's
/// alone.
/// Asks, then applies. The confirmation spells out the two things that make this
/// more than a display toggle: it reaches the whole cluster, and it needs superuser.
fn toggle_track_io_timing(mut cs: Signal<StatsState>, path: String, enabled: bool) {
    let question = if enabled {
        format!(
            "Turn track_io_timing ON?\n\nThis runs, on the server behind {}:\n\n  \
             ALTER SYSTEM SET track_io_timing = on;\n  SELECT pg_reload_conf();\n\n\
             It applies to the WHOLE Postgres server — every database on that cluster, \
             including any this server is not configured for — and it needs superuser. \
             No restart; the change takes effect on reload.",
            path
        )
    } else {
        format!(
            "Turn track_io_timing OFF?\n\nThis runs, on the server behind {}:\n\n  \
             ALTER SYSTEM SET track_io_timing = off;\n  SELECT pg_reload_conf();\n\n\
             The I/O wait figures will stop being measured for every database on that \
             cluster.",
            path
        )
    };

    if !crate::storage::confirm(question.as_str()) {
        return;
    }

    cs.write().io_timing_busy = Some(path.clone());

    spawn(async move {
        let outcome = match crate::api::set_track_io_timing(path.as_str(), enabled).await {
            // Postgres refusing is an answer worth showing verbatim, not a failure.
            Ok(result) if result.ok => None,
            Ok(result) => Some(
                result
                    .error
                    .unwrap_or_else(|| "Postgres refused, without saying why.".to_string()),
            ),
            Err(err) => Some(err.to_string()),
        };

        {
            let mut write = cs.write();
            write.io_timing_busy = None;
            write.io_timing_error = outcome;
        }

        // The badge, the tiles and the banner all read the collected capabilities, so
        // the page only tells the truth again once the server has been re-read.
        poll_once(cs).await;
    });
}

#[component]
fn DiskIoCard(
    cs: Signal<StatsState>,
    path: String,
    io: DiskIo,
    track_io_timing: Option<bool>,
) -> Element {
    let busy = cs.read().io_timing_busy.as_deref() == Some(path.as_str());
    let failure = cs.read().io_timing_error.clone();
    if !section::is_ready(&io.state) {
        return rsx! {
            div { class: "card",
                div { class: "card__header",
                    span { class: "card__title", "Disk I/O" }
                }
                div { class: "card__body",
                    SectionNotice { state: io.state.clone(), reason: io.reason.clone() }
                }
            }
        };
    }

    let rows: Vec<Element> = io
        .tables
        .iter()
        .map(|table| {
            let cold = table.cache_hit_ratio.map(|ratio| ratio < 0.9).unwrap_or(false);

            rsx! {
                tr { key: "{table.full_name()}",
                    td {
                        span { class: "mono dt-ellipsis", style: "max-width: 260px;", title: "{table.full_name()}",
                            "{table.full_name()}"
                        }
                    }
                    td { class: "mono num", "{fmt::bytes(table.delta_read_bytes)}" }
                    td { class: "mono num",
                        match table.read_bytes_per_sec {
                            Some(rate) => fmt::bytes(Some(rate as i64)),
                            None => fmt::NONE.to_string(),
                        }
                    }
                    td {
                        class: if cold { "mono num state--bad-text" } else { "mono num" },
                        title: "Share of this table's block accesses served from shared buffers",
                        "{fmt::ratio(table.cache_hit_ratio)}"
                    }
                    td { class: "mono num", "{fmt::bytes(table.heap_read_blocks.map(|b| b * 8192))}" }
                    td { class: "mono num", "{fmt::bytes(table.index_read_blocks.map(|b| b * 8192))}" }
                    td { class: "mono num", "{fmt::bytes(table.total_read_blocks.map(|b| b * 8192))}" }
                }
            }
        })
        .collect();

    let reads = if io.tables.is_empty() {
        rsx! {
            div { class: "card__body",
                p { class: "muted", style: "margin: 0; font-size: 12.5px;",
                    "No table in this database has read a block from outside shared buffers."
                }
            }
        }
    } else {
        rsx! {
            table { class: "dt",
                thead {
                    tr {
                        th { "Table" }
                        th { class: "num", title: "Read from outside shared buffers since the previous minute's tick", "Read (last tick)" }
                        th { class: "num", "Per second" }
                        th { class: "num", "Cache hit" }
                        th { class: "num", title: "Lifetime, since the statistics were reset", "Heap" }
                        th { class: "num", title: "Lifetime", "Indexes" }
                        th { class: "num", title: "Lifetime heap + indexes + TOAST", "Total" }
                    }
                }
                tbody { {rows.into_iter()} }
            }
        }
    };

    rsx! {
        div { class: "card",
            div { class: "card__header",
                span { class: "card__title", "Disk I/O" }
                span { class: "card__subtitle",
                    if io.io_timing_enabled == Some(true) { "timed" } else { "untimed" }
                }
            }

            if track_io_timing == Some(false) {
                div { class: "card__body", style: "padding-bottom: 0;",
                    div { class: "banner banner--warn",
                        p { style: "margin: 0 0 6px;",
                            code { class: "mono", "track_io_timing" }
                            " is off, so the I/O wait tiles above read "
                            b { "—" }
                            " rather than a number. The block counts below still work — they say "
                            "how much was read, just not how long it took."
                        }
                        p { style: "margin: 0 0 6px;",
                            "Turn it on as superuser — it is a reload parameter, no restart:"
                        }
                        pre { class: "banner__code mono",
                            "ALTER SYSTEM SET track_io_timing = on;\nSELECT pg_reload_conf();"
                        }
                        p { style: "margin: 6px 0 0;",
                            "On RDS and Cloud SQL set it in the parameter group instead. "
                            "The cost of timing depends on the machine's clock source: on Linux, "
                            code { class: "mono", "tsc" }
                            " is cheap — check "
                            code { class: "mono", "/sys/devices/system/clocksource/clocksource0/current_clocksource" }
                            ", and measure with "
                            code { class: "mono", "pg_test_timing" }
                            " if it says anything else. Figures appear one tick after enabling, "
                            "since these are differences between samples."
                        }

                        div { class: "banner__actions",
                            button {
                                class: "btn btn--primary btn--sm",
                                disabled: busy,
                                onclick: {
                                    let path = path.clone();
                                    move |_| toggle_track_io_timing(cs, path.clone(), true)
                                },
                                if busy { "Working…" } else { "Enable it" }
                            }
                            span { class: "faint", style: "font-size: 11.5px;",
                                "Asks for confirmation first — this reaches the whole server."
                            }
                        }

                        if let Some(failure) = failure.clone() {
                            p { class: "banner__failure mono", "{failure}" }
                        }
                    }
                }
            }

            if track_io_timing == Some(true) {
                div { class: "card__body", style: "padding-bottom: 0; display: flex; align-items: center; gap: 12px; flex-wrap: wrap;",
                    StatePill { label: "track_io_timing on".to_string(), tone: StateTone::Ok }
                    button {
                        class: "btn btn--ghost btn--sm",
                        disabled: busy,
                        onclick: {
                            let path = path.clone();
                            move |_| toggle_track_io_timing(cs, path.clone(), false)
                        },
                        if busy { "Working…" } else { "Disable" }
                    }
                    if let Some(failure) = failure.clone() {
                        span { class: "mono", style: "color: var(--danger); font-size: 11.5px;", "{failure}" }
                    }
                }
            }

            div { class: "card__body", style: "padding-bottom: 0;",
                p { class: "muted", style: "margin: 0; font-size: 12.5px;",
                    "A \"read\" here means "
                    b { "not served from shared buffers" }
                    " — the page may still have come from the operating system's cache without the "
                    "disk moving. That is why the time above matters more than the volume below."
                }
            }

            {reads}

            if let Some(writes) = io.writes.clone() {
                WriteIoRow { writes }
            } else if let Some(reason) = io.writes_unavailable.clone() {
                div { class: "card__body",
                    SectionNotice { state: section::UNAVAILABLE.to_string(), reason: Some(reason) }
                }
            }
        }
    }
}

/// The write side — cluster-wide, and stated as such.
#[component]
fn WriteIoRow(writes: WriteIo) -> Element {
    let forced = writes.forced_checkpoint_ratio();

    // The share of WAL that is whole pages copied rather than the change itself.
    let fpi_note = match (writes.wal_full_page_images, writes.wal_records) {
        (Some(fpi), Some(records)) if records > 0 => {
            format!("{} of {} records", fmt::group(fpi), fmt::group(records))
        }
        _ => fmt::NONE.to_string(),
    };

    rsx! {
        div { class: "card__body",
            p { class: "card__section-title", "Writes — whole server, not just this database" }
            div { class: "tiles",
                StatTile {
                    label: "WAL".to_string(),
                    value: match writes.wal_bytes_per_sec {
                        Some(rate) => fmt::bytes(Some(rate as i64)),
                        None => fmt::NONE.to_string(),
                    },
                    hint: "per second".to_string(),
                    tone: StateTone::Neutral,
                    title: Some("Write-ahead log produced per second, across the whole server.".to_string()),
                }
                StatTile {
                    label: "Full-page images".to_string(),
                    value: fmt::int(writes.wal_full_page_images),
                    hint: fpi_note,
                    tone: StateTone::Neutral,
                    title: Some(
                        "The first write to a page after each checkpoint copies the whole 8 kB page into WAL. A large share of records means checkpoints are too frequent for the write rate, and the WAL volume above is mostly copies rather than changes."
                            .to_string(),
                    ),
                }
                StatTile {
                    label: "Forced checkpoints".to_string(),
                    value: fmt::ratio(forced),
                    hint: format!(
                        "{} of {}",
                        fmt::int(writes.checkpoints_requested),
                        fmt::int(
                            writes
                                .checkpoints_requested
                                .zip(writes.checkpoints_timed)
                                .map(|(requested, timed)| requested + timed)
                        )
                    ),
                    tone: match forced {
                        Some(ratio) if ratio >= 0.5 => StateTone::Bad,
                        Some(ratio) if ratio >= 0.33 => StateTone::Warn,
                        Some(_) => StateTone::Ok,
                        None => StateTone::Neutral,
                    },
                    title: Some(
                        "Checkpoints forced early because WAL hit max_wal_size, rather than run on the timer. Much above a third means max_wal_size is too small — and every forced checkpoint restarts the full-page-image cost."
                            .to_string(),
                    ),
                }
                StatTile {
                    label: "Backend writes".to_string(),
                    value: fmt::int(writes.buffers_written_by_backends),
                    hint: "buffers".to_string(),
                    tone: match writes.buffers_written_by_backends {
                        Some(count) if count > 0 => StateTone::Warn,
                        _ => StateTone::Neutral,
                    },
                    title: Some(
                        "Buffers a query backend had to write itself because no clean buffer was free — a query stalling to do the writer's job. Shows — on Postgres 17+, where the counter moved to pg_stat_io."
                            .to_string(),
                    ),
                }
            }
        }
    }
}

/// The last minute: how many statements ran, how long they took on average, and the
/// longest one.
#[component]
fn ThroughputCard(
    throughput: Option<Throughput>,
    minutes: Vec<MinuteSeries>,
    from_unix_ms: i64,
    to_unix_ms: i64,
) -> Element {
    let Some(throughput) = throughput else {
        return rsx! {
            div { class: "card",
                div { class: "card__header",
                    span { class: "card__title", "Throughput" }
                }
                div { class: "card__body",
                    p { class: "muted", style: "margin: 0; font-size: 12.5px;",
                        "Not measured yet — a minute needs two readings, so the first figure "
                        "appears about two minutes after start-up. Needs pg_stat_statements."
                    }
                }
            }
        };
    };

    rsx! {
        div { class: "card",
            div { class: "card__header",
                span { class: "card__title", "Throughput" }
                span { class: "card__subtitle", "{throughput.window_label()}" }
            }
            div { class: "card__body",
                div { class: "tiles",
                    StatTile {
                        label: "Queries".to_string(),
                        value: fmt::int(throughput.calls),
                        hint: match throughput.calls_per_sec {
                            Some(rate) => format!("{:.1}/s", rate),
                            None => fmt::NONE.to_string(),
                        },
                        tone: StateTone::Neutral,
                        title: Some(
                            "Statements that completed in the window, across the whole database — not just the ones this server's agent ran. Counted by pg_stat_statements, so exact."
                                .to_string(),
                        ),
                    }
                    StatTile {
                        label: "Average time".to_string(),
                        value: fmt::millis(throughput.avg_exec_ms),
                        hint: "per query".to_string(),
                        tone: match throughput.avg_exec_ms {
                            Some(ms) if ms >= 100.0 => StateTone::Warn,
                            Some(_) => StateTone::Ok,
                            None => StateTone::Neutral,
                        },
                        title: Some(
                            "Total execution time in the window divided by the number of statements. Exact for the window."
                                .to_string(),
                        ),
                    }
                    StatTile {
                        label: "Longest seen".to_string(),
                        value: fmt::seconds(throughput.longest_secs),
                        hint: "sampled".to_string(),
                        tone: match throughput.longest_secs {
                            Some(secs) if secs >= 60.0 => StateTone::Bad,
                            Some(secs) if secs >= 5.0 => StateTone::Warn,
                            Some(_) => StateTone::Ok,
                            None => StateTone::Neutral,
                        },
                        title: Some(
                            "Longest execution the 5-second sampler actually saw. This is a floor, not a maximum: a statement that starts and finishes between two samples is never observed — but the ones that get missed are the fast ones."
                                .to_string(),
                        ),
                    }
                    StatTile {
                        label: "Total time".to_string(),
                        value: fmt::millis(throughput.total_exec_ms),
                        hint: "all queries".to_string(),
                        tone: StateTone::Neutral,
                        title: Some(
                            "Execution time of every statement in the window added together. What the average is derived from."
                                .to_string(),
                        ),
                    }
                }

                if let Some(query) = throughput.longest_query.clone() {
                    p { class: "throughput__query",
                        span { class: "throughput__query-label", "Longest:" }
                        span { class: "mono", title: "{query}", "{query}" }
                    }
                }

                if let Some(ms) = throughput.slowest_finished_ms {
                    p { class: "throughput__record",
                        "New slowest execution on record this window: "
                        b { class: "mono", "{fmt::millis(Some(ms))}" }
                        if let Some(query) = throughput.slowest_finished_query.clone() {
                            span { class: "mono", title: "{query}", " — {query}" }
                        }
                    }
                }

                div { style: "margin-top: 16px;",
                    MinuteCharts {
                        series: minutes,
                        from_unix_ms,
                        to_unix_ms,
                        empty_note: "No minutes recorded yet — the first row appears about two minutes after start-up, and the series needs pg_stat_statements."
                            .to_string(),
                    }
                }
            }
        }
    }
}

#[component]
fn LongestQueriesCard(activity: Activity) -> Element {
    if !section::is_ready(&activity.state) {
        return rsx! {
            div { class: "card",
                div { class: "card__header",
                    span { class: "card__title", "Longest running" }
                }
                div { class: "card__body",
                    SectionNotice { state: activity.state.clone(), reason: activity.reason.clone() }
                }
            }
        };
    }

    if activity.longest.is_empty() {
        return rsx! {
            div { class: "card",
                div { class: "card__header",
                    span { class: "card__title", "Longest running" }
                    span { class: "card__subtitle", "nothing active" }
                }
                div { class: "card__body",
                    p { class: "muted", style: "margin: 0; font-size: 12.5px;",
                        "No statement is executing on this database right now."
                    }
                }
            }
        };
    }

    let rows: Vec<Element> = activity
        .longest
        .iter()
        .map(|query| {
            rsx! {
                tr { key: "{query.pid:?}",
                    td { class: "mono num", "{fmt::int(query.pid)}" }
                    td { class: "mono", "{fmt::seconds(query.running_secs)}" }
                    td {
                        span { class: "mono dt-ellipsis", style: "max-width: 160px;", title: "{query.who()}",
                            "{query.who()}"
                        }
                    }
                    td { class: "mono", "{query.wait.clone().unwrap_or_else(|| fmt::NONE.to_string())}" }
                    td {
                        span { class: "mono dt-ellipsis", style: "max-width: 420px;", title: "{query.query_label()}",
                            "{query.query_label()}"
                        }
                    }
                }
            }
        })
        .collect();

    rsx! {
        div { class: "card",
            div { class: "card__header",
                span { class: "card__title", "Longest running" }
                span { class: "card__subtitle", "this database only" }
            }
            table { class: "dt",
                thead {
                    tr {
                        th { class: "num", "PID" }
                        th { "For" }
                        th { "Who" }
                        th { title: "What the backend is blocked on, if anything", "Wait" }
                        th { "Statement" }
                    }
                }
                tbody { {rows.into_iter()} }
            }
        }
    }
}

#[component]
fn DatabaseSection(cs: Signal<StatsState>, db: DatabaseStats) -> Element {
    let stats = db.stats;

    rsx! {
        section { class: "db-stats",
            div { class: "db-stats__head",
                div { class: "db-stats__title",
                    span { class: "db-row__desc", "{db.description}" }
                    span { class: "db-row__path mono", "{db.path}" }
                }
                span { class: "db-stats__collected mono faint",
                    "live {fmt::time(stats.collected_at.as_deref())} · tables {fmt::time(stats.slow_collected_at.as_deref())}"
                }
            }

            ServerBadges { server: stats.server.clone() }

            if let Some(error) = stats.last_error.clone() {
                div { class: "banner banner--bad",
                    b { "This database could not be reached. " }
                    span { class: "mono", "{error}" }
                }
            }

            LoadTiles { health: stats.health.clone(), activity: stats.activity.clone() }

            if let Some(count) = stats.activity.state_unknown.filter(|count| *count > 0) {
                div { class: "banner banner--warn",
                    "{count} backend(s) are invisible to this account, so the connection "
                    "breakdown above is an undercount. Grant "
                    code { class: "mono", "pg_monitor" }
                    " to see them."
                }
            }

            ThroughputCard {
                throughput: stats.throughput.clone(),
                minutes: cs
                    .read()
                    .minutes
                    .iter()
                    .find(|(path, _)| path == &db.path)
                    .map(|(_, series)| series.clone())
                    .unwrap_or_default(),
                from_unix_ms: cs.read().minutes_from_ms,
                to_unix_ms: cs.read().minutes_to_ms,
            }
            LoadCard {
                cs,
                path: db.path.clone(),
                load: stats.load.clone(),
                server: stats.server.clone(),
            }
            DiskIoCard {
                cs,
                path: db.path.clone(),
                io: stats.disk_io.clone(),
                track_io_timing: stats.server.track_io_timing,
            }
            TablesCard { tables: stats.tables.clone() }
            LongestQueriesCard { activity: stats.activity.clone() }
        }
    }
}

#[component]
fn HistoryBar(history: HistoryInfo, error: Option<String>) -> Element {
    let tone = if error.is_some() {
        StateTone::Bad
    } else if history.is_healthy() {
        StateTone::Ok
    } else {
        StateTone::Warn
    };

    rsx! {
        div { class: "card",
            div { class: "card__header",
                span { class: "card__title", "Metrics history" }
                span { class: "card__subtitle mono", "{history.path}" }
            }
            div { class: "card__body", style: "display: flex; align-items: center; gap: 12px; flex-wrap: wrap;",
                StatePill { label: history.label(), tone }
                p { class: "muted", style: "margin: 0; font-size: 12.5px;",
                    "Load samples are recorded every 5 s; table sizes and statement costs hourly. "
                    "Anything past the retention horizon is deleted on the hourly sweep. Read it at "
                    code { class: "mono", "GET /api/Stats/History?path=…&hours=…&section=load|tables|statements" }
                    "."
                }
                if let Some(error) = error {
                    span { class: "mono", style: "color: var(--danger);", "{error}" }
                }
            }
        }
    }
}

/// Switches between Postgres servers.
///
/// Rendered only when there is more than one: several `databases:` entries usually
/// point at the same server, and with one server there is nothing to switch — a
/// lone tab would be a control that does nothing.
#[component]
fn ServerSwitch(cs: Signal<StatsState>, servers: Vec<ServerRef>, current: String) -> Element {
    if servers.len() < 2 {
        return rsx! {};
    }

    let tabs: Vec<Element> = servers
        .iter()
        .map(|server| {
            let key = server.key.clone();
            let is_active = key == current;
            let databases = cs.read().stats.databases_on(key.as_str()).len();

            rsx! {
                button {
                    key: "{server.key}",
                    class: if is_active { "server-tab server-tab--active" } else { "server-tab" },
                    onclick: move |_| select_server(cs, key.clone()),
                    span { class: "server-tab__label", "{server.label}" }
                    span { class: "server-tab__count mono", "{databases}" }
                }
            }
        })
        .collect();

    rsx! {
        div { class: "server-switch",
            span { class: "server-switch__caption", "Server" }
            div { class: "server-switch__tabs", {tabs.into_iter()} }
        }
    }
}

/// Selects a server, remembers it, and redraws its panels straight away rather than
/// leaving the previous server's on screen until the 30-second clock comes round.
fn select_server(mut cs: Signal<StatsState>, key: String) {
    crate::storage::save_selected_server(key.as_str());

    {
        let mut write = cs.write();
        write.selected_server = Some(key);
        // Clear rather than keep: the panels on screen belong to the server being
        // left, and showing them under the new heading would be wrong for as long as
        // the fetch takes.
        write.chart = Vec::new();
        write.chart_server = None;
        write.chart_note = "Loading…".to_string();
    }

    spawn(async move {
        refresh_chart(cs).await;
    });
}

#[component]
pub fn Stats() -> Element {
    let cs = use_signal(StatsState::new);

    use_hook(move || {
        // The last server looked at, so a reload does not drop the operator back on
        // whichever one the settings file happens to declare first.
        if let Some(saved) = crate::storage::load_selected_server() {
            cs.clone().write().selected_server = Some(saved);
        }

        spawn(async move {
            // The first stats poll is what names a server, so the panels are fetched
            // right after it rather than on their own clock — otherwise the card sits
            // empty for the first 30 seconds of every visit.
            poll_once(cs).await;
            refresh_chart(cs).await;

            loop {
                dioxus_utils::js::sleep(POLL_INTERVAL).await;
                poll_once(cs).await;
            }
        });

        spawn(async move {
            loop {
                dioxus_utils::js::sleep(CHART_REFRESH).await;
                refresh_chart(cs).await;
            }
        });
    });

    let cs_ra = cs.read();

    let writes_enabled_count = cs_ra.settings.writes_enabled_count();
    let databases_count = cs_ra.settings.databases.len();

    let servers = cs_ra.stats.servers();
    let current = cs_ra.current_server();
    let current_key = current
        .as_ref()
        .map(|server| server.key.clone())
        .unwrap_or_default();

    let shown: Vec<DatabaseStats> = match current.as_ref() {
        Some(server) => cs_ra.stats.databases_on(server.key.as_str()),
        None => Vec::new(),
    };

    let sections: Vec<Element> = shown
        .iter()
        .map(|db| {
            rsx! {
                DatabaseSection { key: "{db.path}", cs, db: db.clone() }
            }
        })
        .collect();

    let body = if sections.is_empty() {
        rsx! {
            div { class: "card",
                div { class: "card__body",
                    p { class: "muted", style: "margin: 0; font-size: 12.5px;",
                        "No database is configured, or the server is unreachable."
                    }
                }
            }
        }
    } else {
        rsx! {
            {sections.into_iter()}
        }
    };

    let chart_subtitle = match current.as_ref() {
        Some(server) => format!("last hour · {}", server.label),
        None => "last hour".to_string(),
    };

    rsx! {
        div { class: "shell",
            Topbar { writes_enabled_count, databases_count }
            section { class: "page page--padded",
                div { style: "display: flex; flex-direction: column; gap: 18px;",

                    ServerSwitch { cs, servers, current: current_key }

                    div { class: "card",
                        div { class: "card__header",
                            span { class: "card__title", "Execution load" }
                            span { class: "card__subtitle", "{chart_subtitle}" }
                        }
                        div { class: "card__body",
                            p { class: "muted", style: "margin: 0 0 12px; font-size: 12.5px;",
                                "Backend-seconds of execution per wall-clock second, "
                                b { "per database" }
                                " — Postgres keeps this in "
                                code { class: "mono", "pg_stat_database" }
                                ", which has a row per database and no cluster-wide equivalent, so "
                                "there is nothing here to add up. Not a CPU figure: a backend waiting "
                                "on disk or on a lock still counts as executing."
                            }
                            LoadCharts {
                                series: cs_ra.chart.clone(),
                                from_unix_ms: cs_ra.chart_from_ms,
                                to_unix_ms: cs_ra.chart_to_ms,
                                empty_note: cs_ra.chart_note.clone(),
                            }
                        }
                    }

                    HistoryBar { history: cs_ra.stats.history.clone(), error: cs_ra.error.clone() }
                    {body}
                }
            }
        }
    }
}
