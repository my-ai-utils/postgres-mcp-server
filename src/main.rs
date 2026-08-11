use std::sync::Arc;

mod app;
mod db_stats;
mod http_server;
mod mcp_service;
mod postgres;
mod settings;
mod sql_guard;
mod sql_log;

#[tokio::main]
async fn main() {
    let settings_file_name = format!("~/.{}", env!("CARGO_PKG_NAME"));
    let settings = crate::settings::SettingsReader::new(settings_file_name.as_str()).await;

    let settings = Arc::new(settings);

    // Resolved once, up front: a bad path or a missing connection string has to
    // stop the boot rather than surface as a dead endpoint later.
    let mounts = match settings
        .use_settings(|settings| settings.get_mounts())
        .await
    {
        Ok(mounts) => mounts,
        Err(err) => panic!("Invalid settings in '{}'. {}", settings_file_name, err),
    };

    for mount in &mounts {
        println!("MCP endpoint {} -> {}", mount.path, mount.description);
    }

    let app_ctx = crate::app::AppContext::new(settings, mounts).await;

    let app_ctx = Arc::new(app_ctx);

    crate::http_server::setup_server(&app_ctx).await;

    // Started after the port is bound: the first tick reaches out to every
    // database, and a slow one must not hold up the UI coming up.
    crate::db_stats::collector::start(&app_ctx);

    app_ctx.app_states.wait_until_shutdown().await;
}
