use std::sync::Arc;

use rust_extensions::AppStates;

use crate::settings::{DatabaseMount, SettingsReader};

use super::DbContext;

pub const APP_NAME: &'static str = env!("CARGO_PKG_NAME");
pub const APP_VERSION: &'static str = env!("CARGO_PKG_VERSION");

pub struct AppContext {
    pub app_states: Arc<AppStates>,

    /// One entry per configured database, in the order the settings file
    /// declares them — which is the order the UI lists them in. Fixed at
    /// startup: mounting a database means registering an HTTP middleware, so a
    /// new one needs a restart.
    pub databases: Vec<Arc<DbContext>>,

    /// One log for all databases, so the UI can show a single chronological
    /// timeline; each entry carries the path it ran against.
    pub sql_log: crate::sql_log::SqlRequestsLog,
}

impl AppContext {
    pub async fn new(settings: Arc<SettingsReader>, mounts: Vec<DatabaseMount>) -> Self {
        let mut databases = Vec::with_capacity(mounts.len());

        for mount in mounts {
            databases.push(Arc::new(DbContext::new(mount, settings.clone()).await));
        }

        Self {
            app_states: Arc::new(AppStates::create_initialized()),
            databases,
            sql_log: crate::sql_log::SqlRequestsLog::new(),
        }
    }

    /// Looks a database up by mount path, the way the MCP middleware routes it.
    /// `None` means the caller sent a path that is not configured.
    pub fn get_db(&self, path: &str) -> Option<&Arc<DbContext>> {
        let path = crate::settings::normalize_path(path);

        self.databases
            .iter()
            .find(|db| crate::settings::paths_are_equal(db.path.as_str(), path.as_str()))
    }
}
