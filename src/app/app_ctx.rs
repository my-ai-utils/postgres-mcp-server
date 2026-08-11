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

    /// Three days of metrics history, shared by every database — one file and one
    /// commit per tick rather than one per mount. Each row carries the mount path
    /// it belongs to.
    pub metrics: crate::db_stats::MetricsStore,

    /// The last failure from a history write or retention sweep. Those run on a
    /// timer with no request to fail, so without this a full disk would stop
    /// recording while every live card kept updating.
    pub metrics_write_error: crate::db_stats::LastError,
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
            // A history file that cannot be opened disables history and is
            // reported, but never stops the boot: proxying SQL and gating writes do
            // not depend on it.
            metrics: crate::db_stats::MetricsStore::open().await,
            metrics_write_error: crate::db_stats::LastError::new(),
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
