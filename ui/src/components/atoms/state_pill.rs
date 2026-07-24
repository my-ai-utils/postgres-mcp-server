use dioxus::prelude::*;

#[derive(Clone, Copy, PartialEq)]
pub enum StateTone {
    Ok,
    Warn,
    Bad,
    Neutral,
}

impl StateTone {
    fn class(&self) -> &'static str {
        match self {
            StateTone::Ok => "state--ok",
            StateTone::Warn => "state--warn",
            StateTone::Bad => "state--bad",
            StateTone::Neutral => "state--neutral",
        }
    }
}

#[component]
pub fn StatePill(
    label: String,
    tone: StateTone,
    /// Hover text — e.g. why a request was refused.
    #[props(default = None)]
    title: Option<String>,
) -> Element {
    rsx! {
        span { class: "state {tone.class()}", title,
            span { class: "state__dot" }
            span { "{label}" }
        }
    }
}
