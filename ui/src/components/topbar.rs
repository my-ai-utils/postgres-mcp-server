use dioxus::prelude::*;

use crate::components::atoms::{Icon, IconKind, StatePill, StateTone};
use crate::storage;

/// Same mark as the browser favicon — see `main.rs`.
const LOGO: Asset = asset!("/public/favicon.svg");

#[component]
pub fn Topbar(writes_enabled: bool) -> Element {
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

    let (writes_label, writes_tone) = if writes_enabled {
        ("writes on", StateTone::Ok)
    } else {
        ("writes off", StateTone::Neutral)
    };

    let theme_icon = if is_dark { IconKind::Sun } else { IconKind::Moon };

    rsx! {
        header { class: "topbar",
            div { class: "topbar__brand",
                img { class: "topbar__logo", src: LOGO, alt: "" }
                span { class: "topbar__brand-name", "Postgres MCP Server" }
            }
            div { class: "topbar__actions",
                StatePill { label: writes_label.to_string(), tone: writes_tone }
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
