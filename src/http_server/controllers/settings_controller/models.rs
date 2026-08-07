use my_http_server::RawDataTyped;
use my_http_server::macros::*;
use serde::{Deserialize, Serialize};

use crate::app::{AppContext, DbContext};

/// One configured database and the state of its write window. No `///`
/// doc-comments on the fields: the MyHttpObjectStructure proc-macro panics on
/// them.
#[derive(Serialize, Deserialize, Debug, Clone, MyHttpObjectStructure)]
pub struct DatabasePublicModel {
    // Mount path of the MCP endpoint, e.g. "/mcp". Identifies the database in
    // every other call.
    pub path: String,
    // What this database is, from the settings file.
    pub description: String,
    #[serde(rename = "mcpWritesEnabled")]
    pub mcp_writes_enabled: bool,
    // `null` while writes are disabled — `MyHttpObjectStructure` rejects
    // `skip_serializing_if`, and the UI mirror reads it as `Option` anyway.
    #[serde(rename = "mcpWritesRemainingSecs")]
    pub mcp_writes_remaining_secs: Option<u64>,
}

impl DatabasePublicModel {
    pub fn new(db: &DbContext) -> Self {
        Self {
            path: db.path.as_str().to_string(),
            description: db.description.clone(),
            mcp_writes_enabled: db.is_mcp_write_enabled(),
            mcp_writes_remaining_secs: db.mcp_writes_remaining_secs(),
        }
    }
}

/// Wire shape of `GET /api/Settings`. Everything here is runtime state — there
/// is nothing persisted to combine it with, so the per-database write windows
/// are the whole model. `Deserialize` is derived only to satisfy `RawDataTyped`'s
/// bound.
#[derive(Serialize, Deserialize, Debug, Clone, MyHttpObjectStructure)]
pub struct SettingsPublicModel {
    pub databases: Vec<DatabasePublicModel>,
}

impl SettingsPublicModel {
    pub fn new(app: &AppContext) -> Self {
        Self {
            databases: app
                .databases
                .iter()
                .map(|db| DatabasePublicModel::new(db))
                .collect(),
        }
    }
}

/// Body for `POST /api/Settings/McpWrites`. `enabled: true` adds 10 minutes to
/// the window of the database at `path` (stacking on whatever is left);
/// `false` resets it to closed.
#[derive(Serialize, Deserialize, Debug, Default, Clone, MyHttpObjectStructure)]
pub struct McpWritesBody {
    // Mount path of the database to change, as returned by GET /api/Settings.
    #[serde(rename = "path")]
    pub path: String,
    #[serde(rename = "enabled")]
    pub enabled: bool,
}

#[derive(MyHttpInput)]
pub struct McpWritesInput {
    #[http_body_raw(
        description = "JSON body { path, enabled }. path is the MCP mount path of the database, e.g. \"/mcp\". enabled=true adds 10 minutes of write access to that database on top of whatever is left; false resets it to off."
    )]
    pub body: RawDataTyped<McpWritesBody>,
}
