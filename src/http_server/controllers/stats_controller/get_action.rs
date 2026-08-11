use std::sync::Arc;

use my_http_server::macros::*;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult, HttpOutput};

use crate::app::AppContext;
use crate::db_stats::StatsModel;

#[http_route(
    method: "GET",
    route: "/api/Stats",
    controller: "Stats",
    description: "Returns the last collected Postgres statistics for every configured database: server version and what the account is allowed to see, connections and long-running queries, pg_stat_database counters with per-window rates, the heaviest statements from pg_stat_statements, and the largest tables. Served from an in-memory cache refreshed by a background poller — reading this endpoint never queries Postgres. Each section carries its own state ('pending' before the first tick, 'unavailable' with a reason when the account or the server version cannot produce it), so a missing extension is distinguishable from an idle database.",
    summary: "Read the collected statistics of every database",
    result:[
        {status_code: 200, description: "Collected statistics", model: "StatsModel"},
    ]
)]
pub struct GetStatsAction {
    app: Arc<AppContext>,
}

impl GetStatsAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &GetStatsAction,
    _ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    // Pure cache read — the collector's timers are the only thing that talks to
    // Postgres, so the 1-second UI poller cannot turn into 1-second catalog
    // queries.
    HttpOutput::as_json(StatsModel::new(action.app.as_ref()))
        .into_ok_result(false)
        .into()
}
