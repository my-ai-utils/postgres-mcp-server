use std::time::Duration;

use dioxus::prelude::*;

use crate::components::Topbar;
use crate::components::atoms::{StatePill, StateTone};
use crate::models::{
    Activity, DatabaseStats, Health, HistoryInfo, Load, ServerInfo, ServerSettings, ServerStats,
    Tables, fmt, section,
};

/// Slower than the requests page's 1s: nothing here moves faster than the
/// collector's 5-second tick, so polling more often would only spend battery
/// re-rendering identical numbers.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Default)]
pub struct StatsState {
    stats: ServerStats,
    /// Only for the topbar's write-access pill. Fetched here rather than passed in
    /// so the pill says the same thing on every page — a topbar that claimed
    /// "writes off" just because this page does not track them would be worse than
    /// one extra small request per tick.
    settings: ServerSettings,
    /// Why the last poll failed, if it did. Kept alongside the previous values
    /// rather than replacing them — a page that blanks on one dropped request is
    /// worse than a page that says "this is a few seconds stale".
    error: Option<String>,
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
                hint: "≈ CPU proxy".to_string(),
                tone: busy_tone(busy),
                title: Some(
                    "Backend-seconds of execution per wall-clock second, from pg_stat_database.active_time. 1.00 means one backend was executing continuously. Postgres exposes no host CPU metric, so this is a proxy: it counts I/O and lock waits as busy and knows nothing about other processes on the machine. Requires Postgres 14+."
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

#[component]
fn LoadCard(load: Load) -> Element {
    let body = if !section::is_ready(&load.state) {
        rsx! {
            div { class: "card__body",
                SectionNotice { state: load.state.clone(), reason: load.reason.clone() }
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
                        th { class: "num", "Dead" }
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
fn DatabaseSection(db: DatabaseStats) -> Element {
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

            LoadCard { load: stats.load.clone() }
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

#[component]
pub fn Stats() -> Element {
    let cs = use_signal(StatsState::default);

    use_hook(move || {
        spawn(async move {
            loop {
                poll_once(cs).await;
                dioxus_utils::js::sleep(POLL_INTERVAL).await;
            }
        });
    });

    let cs_ra = cs.read();

    let writes_enabled_count = cs_ra.settings.writes_enabled_count();
    let databases_count = cs_ra.settings.databases.len();

    let sections: Vec<Element> = cs_ra
        .stats
        .databases
        .iter()
        .map(|db| {
            rsx! {
                DatabaseSection { key: "{db.path}", db: db.clone() }
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

    rsx! {
        div { class: "shell",
            Topbar { writes_enabled_count, databases_count }
            section { class: "page page--padded",
                div { style: "display: flex; flex-direction: column; gap: 18px;",
                    HistoryBar { history: cs_ra.stats.history.clone(), error: cs_ra.error.clone() }
                    {body}
                }
            }
        }
    }
}
