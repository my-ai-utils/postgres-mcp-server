use serde::{Deserialize, Serialize};

/// Mirrors the server's `SettingsPublicModel` (`GET`/`POST /api/Settings`).
///
/// `Default` is all-false on purpose: when the server cannot be reached the
/// card renders "disabled", which is the safe reading of an unknown state.
#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
pub struct ServerSettings {
    #[serde(rename = "mcpWritesEnabled", default)]
    pub mcp_writes_enabled: bool,
    /// Seconds left in the 10-minute window. The server omits the field
    /// entirely (`skip_serializing_if`) while writes are disabled, so this is
    /// `Option` + `default` rather than a plain number.
    #[serde(rename = "mcpWritesRemainingSecs", default)]
    pub mcp_writes_remaining_secs: Option<u64>,
}

impl ServerSettings {
    /// `enabled — ~9m 07s left` / `disabled` — the card subtitle.
    pub fn status_label(&self) -> String {
        if !self.mcp_writes_enabled {
            return "disabled".to_string();
        }

        match self.mcp_writes_remaining_secs {
            Some(secs) => format!("enabled — ~{}m {:02}s left", secs / 60, secs % 60),
            None => "enabled".to_string(),
        }
    }

    pub fn status_color(&self) -> &'static str {
        if self.mcp_writes_enabled {
            "var(--ok)"
        } else {
            "var(--text-muted)"
        }
    }

    /// `9m 07s` — the "Time remaining" row.
    pub fn remaining_label(&self) -> String {
        match self.mcp_writes_remaining_secs {
            Some(secs) => format!("{}m {:02}s", secs / 60, secs % 60),
            None => "—".to_string(),
        }
    }
}

/// Body of `POST /api/Settings/McpWrites`.
#[derive(Serialize)]
pub struct SetMcpWritesRequest {
    pub enabled: bool,
}
