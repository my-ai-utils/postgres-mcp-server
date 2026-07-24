use std::sync::Arc;

use my_http_server::macros::*;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult, HttpOutput};

use crate::app::AppContext;

use super::models::{McpWritesBody, McpWritesInput, SettingsPublicModel};

#[http_route(
    method: "POST",
    route: "/api/Settings/McpWrites",
    controller: "Settings",
    description: "Enables or disables write SQL (INSERT, UPDATE, DELETE, DDL, ...) over the MCP sql_request tool. Each call with enabled=true ADDS 10 minutes on top of whatever is left, so calling it twice grants 20 minutes; the window then auto-closes. enabled=false resets it to closed immediately. Runtime-only — a server restart always leaves writes disabled. Read-only queries are unaffected.",
    summary: "Add 10 minutes of write access, or reset it to off",
    input_data: McpWritesInput,
    result:[
        {status_code: 200, description: "Updated settings", model: "SettingsPublicModel"},
    ]
)]
pub struct SetMcpWritesAction {
    app: Arc<AppContext>,
}

impl SetMcpWritesAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &SetMcpWritesAction,
    input_data: McpWritesInput,
    _ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    let body: McpWritesBody = input_data.body.deserialize_json()?;

    if body.enabled {
        action.app.enable_mcp_writes();
    } else {
        action.app.disable_mcp_writes();
    }

    HttpOutput::as_json(SettingsPublicModel::new(action.app.as_ref()))
        .into_ok_result(false)
        .into()
}
