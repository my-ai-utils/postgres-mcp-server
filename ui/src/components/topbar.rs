use dioxus::prelude::*;

use crate::AppRoute;
use crate::components::atoms::{Icon, IconKind, StatePill, StateTone};
use crate::storage;

/// Same mark as the browser favicon — see `main.rs`.
const LOGO: Asset = asset!("/public/favicon.svg");

/// The pages, in the order they appear in the topbar.
const NAV: [(&str, fn() -> AppRoute); 2] = [
    ("Requests", || AppRoute::Home {}),
    ("Statistics", || AppRoute::Stats {}),
];

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

    // `use_route` rather than a prop: the topbar already knows which page it is on,
    // and threading the answer through every page would be one more thing to get
    // wrong when a page is added.
    let current = use_route::<AppRoute>();

    let nav: Vec<Element> = NAV
        .iter()
        .map(|(label, route)| {
            let route = route();
            let is_active = route == current;

            rsx! {
                Link {
                    key: "{label}",
                    class: if is_active { "topbar__nav-link topbar__nav-link--active" } else { "topbar__nav-link" },
                    to: route,
                    "{label}"
                }
            }
        })
        .collect();

    rsx! {
        header { class: "topbar",
            div { class: "topbar__brand",
                img { class: "topbar__logo", src: LOGO, alt: "" }
                span { class: "topbar__brand-name", "Postgres MCP Server" }
            }
            nav { class: "topbar__nav", {nav.into_iter()} }
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
