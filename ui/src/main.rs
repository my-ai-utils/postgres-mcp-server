use dioxus::prelude::*;

mod api;
mod components;
mod models;
mod pages;
mod storage;

use pages::*;

#[derive(Routable, PartialEq, Clone)]
pub enum AppRoute {
    #[route("/")]
    Home {},
    /// The server's SPA fallback answers index.html for any unmatched path, so a
    /// deep link here survives a reload — see `StaticFilesMiddleware` in
    /// `http_server/startup.rs`.
    #[route("/stats")]
    Stats {},
}

fn main() {
    dioxus::LaunchBuilder::new().launch(|| {
        let theme = storage::load_theme().unwrap_or_else(|| "light".to_string());
        storage::apply_theme(&theme);

        rsx! {
            document::Link {
                rel: "icon",
                r#type: "image/svg+xml",
                href: asset!("/public/favicon.svg"),
            }
            Router::<AppRoute> {}
        }
    });
}
