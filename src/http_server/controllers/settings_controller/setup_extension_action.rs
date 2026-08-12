use std::sync::Arc;

use my_http_server::macros::*;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult, HttpOutput};
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::AppContext;
use crate::db_stats::SetupAction;
use crate::sql_log::SqlRequestStatus;

use super::models::{SetupExtensionInput, SetupExtensionResultModel};

/// Wire value for [`SetupAction::Preload`]; anything else means "create".
const ACTION_PRELOAD: &str = "preload";

#[http_route(
    method: "POST",
    route: "/api/Settings/PgStatStatements",
    controller: "Settings",
    input_data: "SetupExtensionInput",
    description: "Installs pg_stat_statements on the server behind one database. action='create' runs CREATE EXTENSION IF NOT EXISTS and takes effect immediately — this is the whole job when the library is already preloaded, which it is by default on RDS. action='preload' appends the library to shared_preload_libraries, APPENDING to whatever is already there so other extensions are not disabled, and needs a full Postgres RESTART afterwards; a reload is not enough. Both refuse outright when the extension is not in pg_available_extensions, because naming a library that is not on disk stops Postgres from starting. A refusal comes back 200 with 'done': false and the reason.",
    summary: "Install pg_stat_statements",
    result:[
        {status_code: 200, description: "What was done, and whether a restart is needed", model: "SetupExtensionResultModel"},
        {status_code: 404, description: "No database is configured on that path"},
    ]
)]
pub struct SetupExtensionAction {
    app: Arc<AppContext>,
}

impl SetupExtensionAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &SetupExtensionAction,
    input_data: SetupExtensionInput,
    _ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    let body = input_data.body.deserialize_json()?;

    let db = action.app.get_db(body.path.as_str()).ok_or_else(|| {
        HttpFailResult::as_not_found(
            format!("No database is configured on path '{}'.", body.path),
            false,
        )
    })?;

    // Anything that is not the one word means the safe action, so a typo installs the
    // extension rather than rewriting a postmaster setting.
    let requested = if body.action.trim() == ACTION_PRELOAD {
        SetupAction::Preload
    } else {
        SetupAction::Create
    };

    let started = DateTimeAsMicroseconds::now();

    let outcome = crate::db_stats::run(&db.postgres, requested).await;

    let took_micros = DateTimeAsMicroseconds::now()
        .duration_since(started)
        .get_full_micros()
        .max(0) as u64;

    // Logged like the track_io_timing change and for the same reason: a configuration
    // change to the whole server, made by a click, with no trace anywhere would be the
    // worse outcome. Statements that never ran are logged as blocked, not as errors.
    let (sql, status) = match &outcome {
        Ok(outcome) if outcome.sql.is_empty() => (
            format!("-- pg_stat_statements setup: {}", outcome.message),
            SqlRequestStatus::Blocked {
                message: outcome.message.clone(),
            },
        ),
        Ok(outcome) => (outcome.sql.clone(), SqlRequestStatus::Ok { rows: 0 }),
        Err(message) => (
            "-- pg_stat_statements setup".to_string(),
            SqlRequestStatus::Error {
                message: message.clone(),
            },
        ),
    };

    action.app.sql_log.add(
        db.path.clone(),
        sql,
        true,
        started,
        Some(took_micros),
        status,
    );

    let model = match outcome {
        Ok(outcome) => SetupExtensionResultModel {
            done: outcome.done,
            restart_required: outcome.restart_required,
            message: outcome.message,
            error: None,
        },
        // Postgres refusing — "must be superuser", or the statement not being allowed
        // over this connection — is an answer the operator needs to read, not a 500.
        Err(error) => SetupExtensionResultModel {
            done: false,
            restart_required: false,
            message: "The statement did not run.".to_string(),
            error: Some(error),
        },
    };

    HttpOutput::as_json(model).into_ok_result(false).into()
}
