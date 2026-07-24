use std::sync::Arc;

use my_http_server::macros::*;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult, HttpOutput};

use crate::app::AppContext;

use super::models::SettingsPublicModel;

#[http_route(
    method: "GET",
    route: "/api/Settings",
    controller: "Settings",
    description: "Returns the runtime write-access state: whether write SQL is currently allowed over MCP, and how many seconds are left in the window.",
    summary: "Read settings",
    result:[
        {status_code: 200, description: "Settings", model: "SettingsPublicModel"},
    ]
)]
pub struct GetSettingsAction {
    app: Arc<AppContext>,
}

impl GetSettingsAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &GetSettingsAction,
    _ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    HttpOutput::as_json(SettingsPublicModel::new(action.app.as_ref()))
        .into_ok_result(false)
        .into()
}
