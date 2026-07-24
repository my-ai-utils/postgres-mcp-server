use std::time::Duration;

use dioxus::prelude::*;

use crate::components::Topbar;
use crate::components::atoms::{StatePill, StateTone};
use crate::models::{ServerSettings, SqlRequestModel};

/// One struct, one signal for the whole page.
#[derive(Default)]
pub struct HomeState {
    settings: ServerSettings,
    requests: Vec<SqlRequestModel>,
    /// True while an enable/disable call is in flight.
    saving: bool,
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
        self.saving = false;

        if let Some(settings) = settings {
            self.settings = settings;
        }
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

/// Enables/disables MCP writes, then re-reads the authoritative state — the
/// server owns the window and its clock, so the flag we just sent is not
/// trusted and the countdown is never decremented locally.
fn toggle_mcp_writes(mut cs: Signal<HomeState>, enabled: bool) {
    cs.write().saving = true;

    spawn(async move {
        if let Err(err) = crate::api::set_mcp_writes(enabled).await {
            dioxus_utils::console_log(format!("Failed to update MCP writes: {}", err));
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

    let enabled = cs_ra.settings.mcp_writes_enabled;
    let saving = cs_ra.saving;
    let status_label = cs_ra.settings.status_label();
    let status_color = cs_ra.settings.status_color();
    let remaining_label = cs_ra.settings.remaining_label();
    let requests_count = cs_ra.requests.len();

    let rows: Vec<Element> = cs_ra
        .requests
        .iter()
        .map(|r| {
            rsx! {
                tr { key: "{r.id}",
                    td { class: "mono num", "{r.id}" }
                    td { class: "mono", "{r.time_label()}" }
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
                td { class: "dt__empty", colspan: "6", "No requests yet." }
            }
        }
    } else {
        rsx! {
            {rows.into_iter()}
        }
    };

    rsx! {
        div { class: "shell",
            Topbar { writes_enabled: enabled }
            section { class: "page page--padded",
                div { style: "display: flex; flex-direction: column; gap: 14px;",

                    // ----- Write access card -----
                    div { class: "card", style: "max-width: 640px;",
                        div { class: "card__header",
                            span { class: "card__title", "Write access" }
                            span {
                                class: "card__subtitle",
                                style: "color: {status_color};",
                                "{status_label}"
                            }
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
                                "unless this window is open. Every click adds "
                                b { "10 minutes" }
                                " on top of whatever is left, so press it twice for 20. The window "
                                "auto-closes when the time runs out; "
                                b { "Disable" }
                                " resets it to closed right away. A server restart always leaves it closed."
                            }

                            if enabled {
                                div { class: "settings-row",
                                    label { class: "settings-row__label", "Time remaining" }
                                    div { class: "settings-row__field",
                                        span {
                                            style: "color: var(--ok); font-family: var(--font-mono); font-weight: 600; font-size: 15px;",
                                            "{remaining_label}"
                                        }
                                    }
                                }
                            }
                        }
                        div { class: "card__footer", style: "display: flex; justify-content: flex-end; gap: 6px; padding: 10px 14px;",
                            if enabled {
                                button {
                                    class: "btn btn--ghost btn--sm",
                                    disabled: saving,
                                    onclick: move |_| toggle_mcp_writes(cs, false),
                                    "Disable"
                                }
                                button {
                                    class: "btn btn--primary btn--sm",
                                    disabled: saving,
                                    onclick: move |_| toggle_mcp_writes(cs, true),
                                    if saving { "Working…" } else { "+10 min" }
                                }
                            } else {
                                button {
                                    class: "btn btn--primary btn--sm",
                                    disabled: saving,
                                    onclick: move |_| toggle_mcp_writes(cs, true),
                                    if saving { "Working…" } else { "Enable for 10 min" }
                                }
                            }
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
