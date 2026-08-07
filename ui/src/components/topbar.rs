use dioxus::prelude::*;

use crate::components::atoms::{Icon, IconKind, StatePill, StateTone};
use crate::storage;

/// Same mark as the browser favicon — see `main.rs`.
const LOGO: Asset = asset!("/public/favicon.svg");

#[component]
pub fn Topbar(writes_enabled_count: usize, databases_count: usize) -> Element {
    let mut theme = use_signal(|| storage::load_theme().unwrap_or_else(|| "light".to_string()));
    let is_dark = theme.read().as_str() == "dark";

    let toggle_theme = move |_| {
        let next = if theme.peek().as_str() == "dark" {
            "light"
        } else {
            "dark"
        };
        storage::save_theme(next);
        storage::apply_theme(next);
        theme.set(next.to_string());
    };

    // With one database the pill reads as it always did; with several it has to
    // say how many are open, because "writes on" would hide which.
    let (writes_label, writes_tone) = match writes_enabled_count {
        0 => ("writes off".to_string(), StateTone::Neutral),
        _ if databases_count <= 1 => ("writes on".to_string(), StateTone::Ok),
        count => (
            format!("writes on: {} of {}", count, databases_count),
            StateTone::Ok,
        ),
    };

    let theme_icon = if is_dark { IconKind::Sun } else { IconKind::Moon };

    rsx! {
        header { class: "topbar",
            div { class: "topbar__brand",
                img { class: "topbar__logo", src: LOGO, alt: "" }
                span { class: "topbar__brand-name", "Postgres MCP Server" }
            }
            div { class: "topbar__actions",
                StatePill { label: writes_label, tone: writes_tone }
                button {
                    class: "topbar__icon-btn",
                    title: "Toggle theme",
                    onclick: toggle_theme,
                    Icon { kind: theme_icon }
                }
            }
        }
    }
}
