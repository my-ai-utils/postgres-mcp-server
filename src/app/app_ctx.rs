use std::sync::Arc;

use rust_extensions::AppStates;

pub const APP_NAME: &'static str = env!("CARGO_PKG_NAME");

pub struct AppContext {
    pub app_states: Arc<AppStates>,
    pub postgres: crate::postgres::PostgresAccess,
}

impl AppContext {
    pub async fn new(settings: Arc<crate::settings::SettingsReader>) -> Self {
        Self {
            app_states: Arc::new(AppStates::create_initialized()),
            postgres: crate::postgres::PostgresAccess::new(settings).await,
        }
    }
}
