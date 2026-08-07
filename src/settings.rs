use std::sync::Arc;

use my_postgres::PostgresSettings;
use serde::*;

/// Prefixes the MCP endpoints may not take: the admin API, swagger and the
/// static-files root are served by other middlewares, and a mount that shadowed
/// one of them would be a silently broken UI.
const RESERVED_PREFIXES: [&str; 2] = ["/api", "/swagger"];

#[derive(Debug, Serialize, Deserialize, Clone, my_settings_reader::SettingsModel)]
pub struct SettingsModel {
    /// One entry per MCP endpoint. Each gets its own path, its own Postgres
    /// connection and its own write-access window.
    ///
    /// Required, like every field below it: a settings file that does not
    /// declare it fails to parse and the server does not start. Nothing here
    /// has a defensible implied value — a missing connection string is a typo,
    /// not a request for a default.
    pub databases: Vec<DatabaseSettingsModel>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DatabaseSettingsModel {
    /// HTTP path the MCP endpoint answers on, e.g. `/mcp` or `/mcp-reporting`.
    pub path: String,
    /// Standard `tokio_postgres` connection string.
    pub conn_string: String,
    /// What this database is. Shown in the UI and handed to the agent as this
    /// endpoint's MCP instructions — with several databases mounted it is the
    /// only thing telling the model which one it is talking to, which is why it
    /// is required rather than nice-to-have.
    pub description: String,
}

/// One validated `path -> database` mount.
///
/// Built once at startup. The path is normalized here so that everything
/// downstream — the middleware mount, the request log, the HTTP api and the UI
/// — compares and displays the exact same string.
#[derive(Debug, Clone)]
pub struct DatabaseMount {
    pub path: String,
    pub conn_string: String,
    pub description: String,
}

impl SettingsModel {
    /// Resolves the settings file into the list of databases to mount, in the
    /// order it declares them.
    ///
    /// Every failure here is a misconfiguration the operator has to fix, so it
    /// comes back as an error and stops the boot rather than mounting a subset
    /// — a server that came up with one of three databases missing would look
    /// healthy right up to the first query against it.
    pub fn get_mounts(&self) -> Result<Vec<DatabaseMount>, String> {
        if self.databases.is_empty() {
            return Err("'databases' is empty — declare at least one { path, conn_string, \
                        description }."
                .to_string());
        }

        let mut result = Vec::with_capacity(self.databases.len());

        for db in &self.databases {
            result.push(DatabaseMount::new(
                db.path.as_str(),
                db.conn_string.as_str(),
                db.description.as_str(),
            )?);
        }

        // Two mounts on one path would hand the first middleware in the chain
        // every request and leave the second one dead — and since paths are
        // matched case-insensitively, "/Mcp" and "/mcp" are the same mount.
        for (index, mount) in result.iter().enumerate() {
            if result[..index]
                .iter()
                .any(|earlier| paths_are_equal(earlier.path.as_str(), mount.path.as_str()))
            {
                return Err(format!(
                    "Path '{}' is configured more than once. Each database needs its own path.",
                    mount.path
                ));
            }
        }

        Ok(result)
    }

    /// The connection string currently configured for `path`, read from the
    /// live settings. `None` when that path is no longer in the file.
    fn find_conn_string(&self, path: &str) -> Option<String> {
        self.databases
            .iter()
            .find(|db| paths_are_equal(normalize_path(db.path.as_str()).as_str(), path))
            .map(|db| db.conn_string.trim().to_string())
    }
}

impl DatabaseMount {
    fn new(path: &str, conn_string: &str, description: &str) -> Result<Self, String> {
        let path = normalize_path(path);
        let conn_string = conn_string.trim().to_string();
        let description = description.trim().to_string();

        if path.is_empty() || path == "/" {
            return Err(
                "A database path must be something like '/mcp' — '/' is taken by the UI."
                    .to_string(),
            );
        }

        let lower_path = path.to_lowercase();

        if let Some(reserved) = RESERVED_PREFIXES.iter().find(|reserved| {
            lower_path == **reserved || lower_path.starts_with(&format!("{}/", reserved))
        }) {
            return Err(format!(
                "Path '{}' is reserved by the server ('{}' and everything under it).",
                path, reserved
            ));
        }

        if conn_string.is_empty() {
            return Err(format!("Database '{}' has an empty conn_string.", path));
        }

        if description.is_empty() {
            return Err(format!(
                "Database '{}' has an empty description. It is what tells the agent which \
                 database this endpoint is bound to.",
                path
            ));
        }

        Ok(Self {
            path,
            conn_string,
            description,
        })
    }
}

/// `mcp` / `/mcp/` / ` /mcp ` all mean `/mcp`.
pub fn normalize_path(path: &str) -> String {
    let path = path.trim().trim_end_matches('/');

    if path.starts_with('/') {
        return path.to_string();
    }

    format!("/{}", path)
}

/// Mount paths are compared the way the MCP middleware routes them —
/// case-insensitively.
pub fn paths_are_equal(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

/// [`PostgresSettings`] for a single mount.
///
/// The connection string is looked up in the *live* settings by path on every
/// (re)connect, so editing `~/.postgres-mcp-server` is picked up without a
/// restart. A path that has since disappeared from the file falls back to what
/// it was at startup — handing the driver an empty string instead would turn a
/// typo in the settings file into a stream of unexplained connection errors.
pub struct DbConnectionSettings {
    settings: Arc<SettingsReader>,
    path: String,
    conn_string_at_startup: String,
}

impl DbConnectionSettings {
    pub fn new(settings: Arc<SettingsReader>, mount: &DatabaseMount) -> Self {
        Self {
            settings,
            path: mount.path.clone(),
            conn_string_at_startup: mount.conn_string.clone(),
        }
    }
}

#[async_trait::async_trait]
impl PostgresSettings for DbConnectionSettings {
    async fn get_connection_string(&self) -> String {
        let from_settings = self
            .settings
            .use_settings(|settings| settings.find_conn_string(self.path.as_str()))
            .await;

        from_settings
            .filter(|itm| !itm.is_empty())
            .unwrap_or_else(|| self.conn_string_at_startup.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db(path: &str, conn_string: &str) -> DatabaseSettingsModel {
        DatabaseSettingsModel {
            path: path.to_string(),
            conn_string: conn_string.to_string(),
            description: "Test database".to_string(),
        }
    }

    #[test]
    fn mounts_keep_the_declared_order_and_normalize_paths() {
        let settings = SettingsModel {
            databases: vec![db("/first", "host=first"), db("second", "host=second")],
        };

        let mounts = settings.get_mounts().unwrap();

        assert_eq!(mounts.len(), 2);
        assert_eq!(mounts[0].path, "/first");
        // A path without the leading slash is normalized rather than refused.
        assert_eq!(mounts[1].path, "/second");
    }

    #[test]
    fn an_empty_databases_list_is_an_error() {
        let settings = SettingsModel {
            databases: Vec::new(),
        };

        assert!(settings.get_mounts().is_err());
    }

    #[test]
    fn duplicate_paths_are_refused_case_insensitively() {
        let settings = SettingsModel {
            databases: vec![db("/mcp", "host=a"), db("/MCP/", "host=b")],
        };

        assert!(settings.get_mounts().is_err());
    }

    #[test]
    fn reserved_paths_are_refused() {
        for path in ["/", "/api", "/api/Settings", "/swagger"] {
            let settings = SettingsModel {
                databases: vec![db(path, "host=a")],
            };

            assert!(
                settings.get_mounts().is_err(),
                "path '{}' should be refused",
                path
            );
        }
    }

    #[test]
    fn an_empty_conn_string_is_refused() {
        let settings = SettingsModel {
            databases: vec![db("/mcp", "   ")],
        };

        assert!(settings.get_mounts().is_err());
    }

    #[test]
    fn an_empty_description_is_refused() {
        let settings = SettingsModel {
            databases: vec![DatabaseSettingsModel {
                path: "/mcp".to_string(),
                conn_string: "host=a".to_string(),
                description: "  ".to_string(),
            }],
        };

        assert!(settings.get_mounts().is_err());
    }

    #[test]
    fn a_settings_file_without_databases_does_not_parse() {
        // No `#[serde(default)]` anywhere in the model: a missing key is a
        // misconfiguration and has to stop the boot, not be filled in silently.
        let err = my_settings_reader::serde_yaml::from_str::<SettingsModel>("postgres_conn_string: host=legacy\n");
        assert!(err.is_err());

        // ...and neither does an entry that forgot one of the three fields.
        let err = my_settings_reader::serde_yaml::from_str::<SettingsModel>(
            "databases:\n- path: /mcp\n  conn_string: host=a\n",
        );
        assert!(err.is_err());
    }

    #[test]
    fn conn_string_lookup_follows_the_normalized_path() {
        let settings = SettingsModel {
            databases: vec![db("reporting/", "host=reporting")],
        };

        assert_eq!(
            settings.find_conn_string("/reporting"),
            Some("host=reporting".to_string())
        );
        assert_eq!(settings.find_conn_string("/gone"), None);
    }
}
