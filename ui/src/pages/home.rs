use std::time::Duration;

use dioxus::prelude::*;

use crate::components::Topbar;
use crate::components::atoms::{StatePill, StateTone};
use crate::models::{DatabaseSettings, ServerSettings, SqlRequestModel};

/// One struct, one signal for the whole page.
#[derive(Default)]
pub struct HomeState {
    settings: ServerSettings,
    requests: Vec<SqlRequestModel>,
    /// Mount path of the database whose window is being changed right now, so
    /// only that row's buttons go quiet — a slow call on one database must not
    /// freeze the buttons of the others.
    saving: Option<String>,
}

impl HomeState {
    /// Applies one poll tick. Each half is optional: a fetch that failed keeps
    /// the last known value instead of blanking the card.
    fn apply_tick(
        &mut self,
        settings: Option<ServerSettings>,
        requests: Option<Vec<SqlRequestModel>>,
    ) {
        if let Some(settings) = settings {
            self.settings = settings;
        }

        if let Some(requests) = requests {
            self.requests = requests;
        }
    }

    fn finish_save(&mut self, settings: Option<ServerSettings>) {
        self.saving = None;

        if let Some(settings) = settings {
            self.settings = settings;
        }
    }

    fn is_saving(&self, path: &str) -> bool {
        self.saving.as_deref() == Some(path)
    }
}

/// One tick of the poller: both endpoints, then a single write.
async fn poll_once(mut cs: Signal<HomeState>) {
    let settings = match crate::api::get_server_settings().await {
        Ok(settings) => Some(settings),
        Err(err) => {
            dioxus_utils::console_log(format!("Settings poll failed: {}", err));
            None
        }
    };

    let requests = match crate::api::get_requests().await {
        Ok(requests) => Some(requests),
        Err(err) => {
            dioxus_utils::console_log(format!("Requests poll failed: {}", err));
            None
        }
    };

    cs.write().apply_tick(settings, requests);
}

/// Enables/disables MCP writes for one database, then re-reads the
/// authoritative state — the server owns the window and its clock, so the flag
/// we just sent is not trusted and the countdown is never decremented locally.
fn toggle_mcp_writes(mut cs: Signal<HomeState>, path: String, enabled: bool) {
    cs.write().saving = Some(path.clone());

    spawn(async move {
        if let Err(err) = crate::api::set_mcp_writes(path.clone(), enabled).await {
            dioxus_utils::console_log(format!("Failed to update MCP writes for {}: {}", path, err));
            cs.write().finish_save(None);
            return;
        }

        let settings = match crate::api::get_server_settings().await {
            Ok(settings) => Some(settings),
            Err(err) => {
                dioxus_utils::console_log(format!("Settings re-read failed: {}", err));
                None
            }
        };

        cs.write().finish_save(settings);
    });
}

fn status_tone(status: &str) -> StateTone {
    match status {
        "ok" => StateTone::Ok,
        "blocked" => StateTone::Warn,
        "error" => StateTone::Bad,
        _ => StateTone::Neutral,
    }
}

/// One database's row in the Write access card: which database it is, whether
/// its window is open, and the buttons that open or close it. The path is right
/// next to the buttons on purpose — with several databases mounted, "Enable"
/// has to say what it enables.
#[component]
fn WriteAccessRow(cs: Signal<HomeState>, db: DatabaseSettings) -> Element {
    let saving = cs.read().is_saving(db.path.as_str());

    let enabled = db.mcp_writes_enabled;
    let path = db.path.clone();

    let (tone, label) = if enabled {
        (StateTone::Ok, db.status_label())
    } else {
        (StateTone::Neutral, db.status_label())
    };

    let enable_path = path.clone();
    let disable_path = path.clone();
    let add_path = path.clone();

    rsx! {
        div { class: "db-row",
            div { class: "db-row__main",
                span { class: "db-row__desc", "{db.description}" }
                span { class: "db-row__path", "{path}" }
            }

            div { class: "db-row__state",
                StatePill { label, tone }
                if enabled {
                    span { class: "db-row__remaining", "{db.remaining_label()}" }
                }
            }

            div { class: "db-row__actions",
                if enabled {
                    button {
                        class: "btn btn--ghost btn--sm",
                        disabled: saving,
                        onclick: move |_| toggle_mcp_writes(cs, disable_path.clone(), false),
                        "Disable"
                    }
                    button {
                        class: "btn btn--primary btn--sm",
                        disabled: saving,
                        onclick: move |_| toggle_mcp_writes(cs, add_path.clone(), true),
                        if saving { "Working…" } else { "+10 min" }
                    }
                } else {
                    button {
                        class: "btn btn--primary btn--sm",
                        disabled: saving,
                        onclick: move |_| toggle_mcp_writes(cs, enable_path.clone(), true),
                        if saving { "Working…" } else { "Enable for 10 min" }
                    }
                }
            }
        }
    }
}

#[component]
pub fn Home() -> Element {
    let cs = use_signal(HomeState::default);

    // One 1s poller drives both the countdown and the table. `use_hook` runs
    // its body exactly once per component instance, so the loop starts once
    // without writing to a signal during render.
    use_hook(move || {
        spawn(async move {
            loop {
                poll_once(cs).await;
                dioxus_utils::js::sleep(Duration::from_secs(1)).await;
            }
        });
    });

    let cs_ra = cs.read();

    let writes_enabled_count = cs_ra.settings.writes_enabled_count();
    let databases_count = cs_ra.settings.databases.len();
    let writes_summary = cs_ra.settings.writes_summary();
    let requests_count = cs_ra.requests.len();

    let database_rows: Vec<Element> = cs_ra
        .settings
        .databases
        .iter()
        .map(|db| {
            rsx! {
                WriteAccessRow { key: "{db.path}", cs, db: db.clone() }
            }
        })
        .collect();

    let databases_body = if database_rows.is_empty() {
        rsx! {
            p { class: "muted", style: "margin: 0; font-size: 12.5px;",
                "No database is configured, or the server is unreachable."
            }
        }
    } else {
        rsx! {
            {database_rows.into_iter()}
        }
    };

    let rows: Vec<Element> = cs_ra
        .requests
        .iter()
        .map(|r| {
            rsx! {
                tr { key: "{r.id}",
                    td { class: "mono num", "{r.id}" }
                    td { class: "mono", "{r.time_label()}" }
                    td {
                        span { class: "mono dt-ellipsis", style: "max-width: 160px;", title: "{r.db}", "{r.db}" }
                    }
                    td {
                        span { class: "mono dt-ellipsis", title: "{r.sql}", "{r.sql}" }
                    }
                    td { class: "mono num", "{r.rows_label()}" }
                    td { class: "mono num", "{r.took_label()}" }
                    td {
                        StatePill {
                            label: r.status.clone(),
                            tone: status_tone(&r.status),
                            title: r.status_title().map(|t| t.to_string()),
                        }
                    }
                }
            }
        })
        .collect();

    let table_body = if rows.is_empty() {
        rsx! {
            tr {
                td { class: "dt__empty", colspan: "7", "No requests yet." }
            }
        }
    } else {
        rsx! {
            {rows.into_iter()}
        }
    };

    rsx! {
        div { class: "shell",
            Topbar { writes_enabled_count, databases_count }
            section { class: "page page--padded",
                div { style: "display: flex; flex-direction: column; gap: 14px;",

                    // ----- Write access card: one row per database -----
                    div { class: "card",
                        div { class: "card__header",
                            span { class: "card__title", "Write access" }
                            span { class: "card__subtitle", "{writes_summary}" }
                        }
                        div { class: "card__body", style: "display: flex; flex-direction: column; gap: 14px;",
                            p { style: "margin: 0; color: var(--text-muted); font-size: 12.5px;",
                                code { style: "font-family: var(--font-mono);", "SELECT" }
                                " and other read-only statements always work. "
                                code { style: "font-family: var(--font-mono);", "INSERT" }
                                ", "
                                code { style: "font-family: var(--font-mono);", "UPDATE" }
                                ", "
                                code { style: "font-family: var(--font-mono);", "DELETE" }
                                ", "
                                code { style: "font-family: var(--font-mono);", "TRUNCATE" }
                                ", DDL and anything else that writes are refused by the MCP tool "
                                "unless that database's window is open. Every database below is a "
                                "separate MCP endpoint with its "
                                b { "own" }
                                " window — enabling one leaves the others closed. Every click adds "
                                b { "10 minutes" }
                                " on top of whatever is left, so press it twice for 20. The window "
                                "auto-closes when the time runs out; "
                                b { "Disable" }
                                " resets it to closed right away. A server restart always leaves "
                                "every database closed."
                            }

                            div { class: "db-list", {databases_body} }
                        }
                    }

                    // ----- SQL requests card -----
                    div { class: "card",
                        div { class: "card__header",
                            span { class: "card__title", "SQL requests" }
                            span { class: "card__subtitle", "last {requests_count}" }
                        }
                        table { class: "dt",
                            thead {
                                tr {
                                    th { class: "num", "#" }
                                    th { "Time" }
                                    th { "Database" }
                                    th { "SQL" }
                                    th { class: "num", "Rows" }
                                    th { class: "num", "Took" }
                                    th { "Status" }
                                }
                            }
                            tbody { {table_body} }
                        }
                    }
                }
            }
        }
    }
}
