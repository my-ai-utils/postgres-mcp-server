use std::sync::Arc;

use my_http_server::macros::*;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult, HttpOutput};
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::AppContext;
use crate::sql_log::SqlRequestStatus;

use super::models::{TrackIoTimingInput, TrackIoTimingResultModel};

#[http_route(
    method: "POST",
    route: "/api/Settings/TrackIoTiming",
    controller: "Settings",
    input_data: "TrackIoTimingInput",
    description: "Turns the server's track_io_timing setting on or off, so the I/O wait figures on the statistics page have something to report. Runs a fixed ALTER SYSTEM followed by pg_reload_conf() — nothing from the request reaches the parser — then reads the setting back from the live session and returns what the server actually says, rather than what was asked for. NOTE: ALTER SYSTEM applies to the whole Postgres server, every database on that cluster, and requires superuser. A refusal comes back 200 with 'ok': false and the driver's own message, because a permission error is an answer, not a broken request.",
    summary: "Enable or disable track_io_timing on the server behind one database",
    result:[
        {status_code: 200, description: "What the server reports after the change", model: "TrackIoTimingResultModel"},
        {status_code: 404, description: "No database is configured on that path"},
    ]
)]
pub struct SetTrackIoTimingAction {
    app: Arc<AppContext>,
}

impl SetTrackIoTimingAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &SetTrackIoTimingAction,
    input_data: TrackIoTimingInput,
    _ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    let body = input_data.body.deserialize_json()?;

    let db = action.app.get_db(body.path.as_str()).ok_or_else(|| {
        HttpFailResult::as_not_found(
            format!("No database is configured on path '{}'.", body.path),
            false,
        )
    })?;

    let started = DateTimeAsMicroseconds::now();
    let sql = crate::db_stats::statements(body.enabled);

    let result = crate::db_stats::set_track_io_timing(&db.postgres, body.enabled).await;

    let took_micros = DateTimeAsMicroseconds::now()
        .duration_since(started)
        .get_full_micros()
        .max(0) as u64;

    // Logged, unlike the collector's queries. The reason those are excluded is volume
    // — a 5-second poller would evict everything else — and that does not apply to a
    // statement someone ran by clicking a button. A configuration change to the whole
    // server with no trace anywhere would be the worse outcome.
    action.app.sql_log.add(
        db.path.clone(),
        sql,
        true,
        started,
        Some(took_micros),
        match &result {
            Ok(_) => SqlRequestStatus::Ok { rows: 0 },
            Err(message) => SqlRequestStatus::Error {
                message: message.clone(),
            },
        },
    );

    // A refusal is an answer, not a failed request: "must be superuser" and "cannot
    // run inside a transaction block" are both things the operator needs to read, and
    // a 500 would bury them in a generic failure.
    let model = match result {
        Ok(enabled) => TrackIoTimingResultModel {
            ok: true,
            enabled: Some(enabled),
            error: None,
        },
        Err(error) => TrackIoTimingResultModel {
            ok: false,
            enabled: None,
            error: Some(error),
        },
    };

    HttpOutput::as_json(model).into_ok_result(false).into()
}
