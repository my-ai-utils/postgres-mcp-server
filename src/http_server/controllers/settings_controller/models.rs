use my_http_server::macros::*;
use my_http_server::types::RawDataTyped;
use serde::{Deserialize, Serialize};

use crate::app::AppContext;

/// Wire shape of `GET /api/Settings`. Everything here is runtime state — there
/// is nothing persisted to combine it with, so the write window is the whole
/// model. `Deserialize` is derived only to satisfy `RawDataTyped`'s bound.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, MyHttpObjectStructure)]
pub struct SettingsPublicModel {
    #[serde(rename = "mcpWritesEnabled")]
    pub mcp_writes_enabled: bool,
    #[serde(
        rename = "mcpWritesRemainingSecs",
        skip_serializing_if = "Option::is_none"
    )]
    pub mcp_writes_remaining_secs: Option<u64>,
}

impl SettingsPublicModel {
    pub fn new(app: &AppContext) -> Self {
        Self {
            mcp_writes_enabled: app.is_mcp_write_enabled(),
            mcp_writes_remaining_secs: app.mcp_writes_remaining_secs(),
        }
    }
}

/// Body for `POST /api/Settings/McpWrites`. `enabled: true` adds 10 minutes to
/// the window (stacking on whatever is left); `false` resets it to closed.
#[derive(Serialize, Deserialize, Debug, Default, Clone, MyHttpObjectStructure)]
pub struct McpWritesBody {
    #[serde(rename = "enabled")]
    pub enabled: bool,
}

#[derive(MyHttpInput)]
pub struct McpWritesInput {
    #[http_body_raw(
        description = "JSON body { enabled }. true adds 10 minutes of write access on top of whatever is left; false resets it to off."
    )]
    pub body: RawDataTyped<McpWritesBody>,
}
