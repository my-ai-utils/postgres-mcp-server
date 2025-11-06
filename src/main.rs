use std::sync::Arc;

mod app;
mod http_server;
mod mcp_service;
mod postgres;
mod settings;

mod mcp_middleware;

#[tokio::main]
async fn main() {
    let settings_file_name = format!("~/.{}", env!("CARGO_PKG_NAME"));
    let settings = crate::settings::SettingsReader::new(settings_file_name.as_str()).await;

    let settings = Arc::new(settings);

    let app_ctx = crate::app::AppContext::new(settings).await;

    let app_ctx = Arc::new(app_ctx);

    crate::http_server::setup_server(&app_ctx).await;

    app_ctx.app_states.wait_until_shutdown().await;
}
