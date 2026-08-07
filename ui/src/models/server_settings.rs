use serde::{Deserialize, Serialize};

/// Mirrors one entry of the server's `SettingsPublicModel.databases` — a single
/// mounted database and the state of *its* write window. Every database is
/// gated separately, so there is no server-wide "writes enabled" flag to mirror.
///
/// Nothing here carries `#[serde(default)]`: a missing `path` or
/// `mcpWritesEnabled` means this mirror has drifted from the server's model, and
/// defaulting it would render a card that claims writes are off for a database
/// with no name. Failing the fetch is the honest outcome.
#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
pub struct DatabaseSettings {
    /// MCP mount path, e.g. `/mcp`. Identifies the database in every call back
    /// to the server.
    pub path: String,
    /// What this database is, from the settings file.
    pub description: String,
    #[serde(rename = "mcpWritesEnabled")]
    pub mcp_writes_enabled: bool,
    /// Seconds left in this database's 10-minute window; `null` while writes
    /// are disabled, hence `Option` rather than a plain number.
    #[serde(rename = "mcpWritesRemainingSecs")]
    pub mcp_writes_remaining_secs: Option<u64>,
}

impl DatabaseSettings {
    /// `enabled — ~9m 07s left` / `disabled`.
    pub fn status_label(&self) -> String {
        if !self.mcp_writes_enabled {
            return "disabled".to_string();
        }

        match self.mcp_writes_remaining_secs {
            Some(secs) => format!("enabled — ~{}m {:02}s left", secs / 60, secs % 60),
            None => "enabled".to_string(),
        }
    }

    /// `9m 07s` — the countdown next to an open window.
    pub fn remaining_label(&self) -> String {
        match self.mcp_writes_remaining_secs {
            Some(secs) => format!("{}m {:02}s", secs / 60, secs % 60),
            None => "—".to_string(),
        }
    }

}

/// Mirrors the server's `SettingsPublicModel` (`GET`/`POST /api/Settings`).
///
/// `Default` is an empty list on purpose: when the server cannot be reached the
/// page shows nothing rather than inventing a database.
#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
pub struct ServerSettings {
    pub databases: Vec<DatabaseSettings>,
}

impl ServerSettings {
    pub fn writes_enabled_count(&self) -> usize {
        self.databases
            .iter()
            .filter(|db| db.mcp_writes_enabled)
            .count()
    }

    /// `2 of 3 enabled` — the card subtitle.
    pub fn writes_summary(&self) -> String {
        format!(
            "{} of {} enabled",
            self.writes_enabled_count(),
            self.databases.len()
        )
    }
}

/// Body of `POST /api/Settings/McpWrites`. The path says which database — the
/// window belongs to one database, never to the server.
#[derive(Serialize)]
pub struct SetMcpWritesRequest {
    pub path: String,
    pub enabled: bool,
}
