use std::sync::Arc;

use my_http_server::macros::*;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult, HttpOutput};

use crate::app::AppContext;

use super::models::{SqlRequestModel, SqlRequestsModel};

#[http_route(
    method: "GET",
    route: "/api/Requests",
    controller: "Requests",
    description: "Returns the last 100 SQL requests executed through the MCP tool, newest first — including the ones the write gate refused. In-memory only: the list is empty after a restart.",
    summary: "Read the last SQL requests",
    result:[
        {status_code: 200, description: "Last SQL requests", model: "SqlRequestsModel"},
    ]
)]
pub struct GetRequestsAction {
    app: Arc<AppContext>,
}

impl GetRequestsAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &GetRequestsAction,
    _ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    // get_all() drops the lock before we touch serialization.
    let items = action.app.sql_log.get_all();

    let model = SqlRequestsModel {
        items: items.iter().map(|itm| SqlRequestModel::new(itm)).collect(),
    };

    HttpOutput::as_json(model).into_ok_result(false).into()
}
