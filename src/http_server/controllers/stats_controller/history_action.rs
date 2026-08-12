use std::sync::Arc;

use my_http_server::macros::*;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult, HttpOutput};

use crate::app::AppContext;

use super::history_models::{
    HistoryInput, HistoryModel, SECTION_LONGEST, SECTION_MINUTES, SECTION_STATEMENTS,
    SECTION_TABLES,
};

#[http_route(
    method: "GET",
    route: "/api/Stats/History",
    controller: "Stats",
    input_data: "HistoryInput",
    description: "Reads the recorded metrics history of one database from the local redb file. 'load' returns the 5-second samples (busy backends, transaction and I/O rates, cache hit ratio, connection counts, database size); 'tables' and 'statements' return the hourly snapshots. History is kept for 3 days and swept hourly. Points are oldest first. A 404 means the path is not a configured mount; a disabled or unreadable history file comes back 200 with an 'error' and empty series, because the mount itself is fine.",
    summary: "Read one database's metrics history",
    result:[
        {status_code: 200, description: "Metrics history", model: "HistoryModel"},
        {status_code: 404, description: "No database is configured on that path"},
    ]
)]
pub struct GetStatsHistoryAction {
    app: Arc<AppContext>,
}

impl GetStatsHistoryAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &GetStatsHistoryAction,
    input_data: HistoryInput,
    _ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    // Resolved through the same lookup the MCP middleware routes with, so "/MCP"
    // and "/mcp/" reach the same series the UI shows.
    let db = action.app.get_db(input_data.path.as_str()).ok_or_else(|| {
        HttpFailResult::as_not_found(
            format!(
                "No database is configured on path '{}'.",
                input_data.path
            ),
            false,
        )
    })?;

    let path = db.path.as_str().to_string();
    let section = input_data.section().to_string();
    let (from, to) = input_data.window();

    let model = HistoryModel::empty(path.clone(), section.as_str(), from, to, None);

    // A history read that fails is reported inside a 200: the request itself was
    // valid and the mount exists — it is the recording that is broken, and the UI
    // shows that next to the live cards rather than as a failed page load.
    let model = match section.as_str() {
        SECTION_TABLES => match action
            .app
            .metrics
            .read_table_sizes(from, to, path.as_str())
            .await
        {
            Ok(rows) => model.with_tables(rows),
            Err(err) => HistoryModel::empty(path, section.as_str(), from, to, Some(err)),
        },
        SECTION_STATEMENTS => match action
            .app
            .metrics
            .read_statements(from, to, path.as_str())
            .await
        {
            Ok(rows) => model.with_statements(rows),
            Err(err) => HistoryModel::empty(path, section.as_str(), from, to, Some(err)),
        },
        SECTION_MINUTES => match action
            .app
            .metrics
            .read_minutes(from, to, path.as_str())
            .await
        {
            Ok(rows) => model.with_minutes(rows),
            Err(err) => HistoryModel::empty(path, section.as_str(), from, to, Some(err)),
        },
        SECTION_LONGEST => match action
            .app
            .metrics
            .read_longest(from, to, path.as_str())
            .await
        {
            Ok(rows) => model.with_longest(rows),
            Err(err) => HistoryModel::empty(path, section.as_str(), from, to, Some(err)),
        },
        _ => match action.app.metrics.read_load(from, to, path.as_str()).await {
            Ok(rows) => model.with_load(rows),
            Err(err) => HistoryModel::empty(path, section.as_str(), from, to, Some(err)),
        },
    };

    HttpOutput::as_json(model).into_ok_result(false).into()
}
