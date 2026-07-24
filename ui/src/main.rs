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
}

fn main() {
    dioxus::LaunchBuilder::new().launch(|| {
        let theme = storage::load_theme().unwrap_or_else(|| "light".to_string());
        storage::apply_theme(&theme);

        rsx! {
            document::Link { rel: "icon", href: asset!("/public/favicon.ico") }
            Router::<AppRoute> {}
        }
    });
}
