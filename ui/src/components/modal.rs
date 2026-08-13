//! A modal dialog, following the pattern `mt-admin` uses.
//!
//! Structure and class names are deliberately the same as
//! `mt-admin/src/dialogs/dialog_template.rs` — `modal-backdrop` → `modal` →
//! `modal-header` / `modal-body` / `modal-footer`, closing on a backdrop click, on
//! the header's × and on the footer's Cancel — so anyone who knows the dialogs there
//! recognises this one. Two things are worth knowing about *why* it is shaped this
//! way rather than a plain `<div>` with a high z-index:
//!
//! - **`stop_propagation` on the panel is what makes the backdrop click safe.**
//!   Without it, every click inside the dialog would bubble to the backdrop and close
//!   it — including a click that selects text, which is the main thing anyone does in
//!   a dialog full of SQL.
//! - **The panel does not scroll; the body does.** The header and footer stay put, so
//!   a 95%-tall dialog holding a long statement still has its close button on screen.
//!
//! Unlike `mt-admin` this project has no `DialogState` context: there is exactly one
//! dialog, so the caller owns an `Option` and this component renders whatever is in
//! it. A context indirection for a single dialog would be structure without purpose.

use dioxus::prelude::*;

/// How large the panel is.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum ModalSize {
    /// The default in `mt-admin`: a column of fields.
    #[default]
    Normal,
    /// 95% of the viewport, both ways — for content that is wide *and* tall, like a
    /// full SQL statement beside its metrics.
    Full,
}

impl ModalSize {
    fn class(&self) -> &'static str {
        match self {
            Self::Normal => "modal",
            Self::Full => "modal modal--full",
        }
    }
}

#[component]
pub fn Modal(
    title: String,
    /// Shown next to the title — what this dialog is about, when the title alone is
    /// not enough.
    #[props(default = String::new())]
    subtitle: String,
    #[props(default = ModalSize::Normal)] size: ModalSize,
    /// Called by the backdrop, the × and Cancel alike. One way out, three affordances.
    on_close: EventHandler<()>,
    children: Element,
) -> Element {
    rsx! {
        div {
            class: "modal-backdrop",
            onclick: move |_| on_close.call(()),

            div {
                class: "{size.class()}",
                // Without this, selecting text inside the dialog would close it.
                onclick: move |evt| evt.stop_propagation(),

                div { class: "modal-header",
                    div { class: "modal-title", "{title}" }
                    if !subtitle.is_empty() {
                        div { class: "modal-subtitle mono", "{subtitle}" }
                    }
                    button {
                        class: "modal-close",
                        title: "Close",
                        onclick: move |_| on_close.call(()),
                        "×"
                    }
                }

                div { class: "modal-body", {children} }

                div { class: "modal-footer",
                    button {
                        class: "btn btn--sm",
                        onclick: move |_| on_close.call(()),
                        "Close"
                    }
                }
            }
        }
    }
}
