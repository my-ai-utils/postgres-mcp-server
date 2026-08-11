//! HTTP access to the Postgres MCP Server admin API.
//!
//! # Two deliberate deviations from `dioxus-client-side-bootstrap`
//!
//! 1. **No `rest-api-shared` crate.** The guide mandates that every wire model
//!    live in a crate shared verbatim by the server and this client, with
//!    request models deriving `MyHttpInput` and calls going through
//!    `.execute_request(HttpVerb::X, model)`. That is impossible here: the
//!    server crate pins `my-http-server` 0.8.3, whose `MyHttpInput` derive is
//!    not wasm-clean, so a crate shared with it cannot compile to
//!    `wasm32-unknown-unknown`. Instead `crate::models` declares **plain-serde
//!    mirror structs** of the server's models and this module calls FlUrl's
//!    `.get()` / `.post(HttpRequestBody::as_json(&body))` directly. The mirrors
//!    are therefore drift-prone by construction, and they are declared strictly
//!    — no `#[serde(default)]` on anything the server always sends — so drift
//!    surfaces as a failed fetch in the console rather than as a page of
//!    zeroes and blanks that looks like real data.
//!
//! 2. **Relative URLs, no origin lookup.** `FlUrl::new("/api/Settings")` — the
//!    wasm fetch backend resolves a root-relative URL against the page origin,
//!    so there is no base URL to thread through.
//!
//! Status handling is centralized in `handle_http_response` / `handle_http_empty`;
//! no call site below checks a status code itself.

use flurl::body::HttpRequestBody;
use flurl::{FlUrl, FlUrlError, FlUrlResponse};
use serde::de::DeserializeOwned;

use crate::models::{
    LoadHistory, RequestError, ServerSettings, ServerStats, SetMcpWritesRequest,
    SetTrackIoTimingRequest, SqlRequestModel, SqlRequestsResponse, TrackIoTimingResult,
};

fn is_success(status: u16) -> bool {
    (200..300).contains(&status)
}

async fn read_error_body(response: &mut FlUrlResponse) -> RequestError {
    let status = response.get_status_code();

    let message = response
        .get_body_as_str()
        .await
        .map(|body| body.to_string())
        .unwrap_or_else(|err| format!("{:?}", err));

    RequestError {
        message: format!("HTTP {}: {}", status, message),
    }
}

/// 2xx → deserialize the body into `T`; any other status → `Err` carrying the body.
async fn handle_http_response<T: DeserializeOwned>(
    response: Result<FlUrlResponse, FlUrlError>,
) -> Result<T, RequestError> {
    let mut response = response?;

    if is_success(response.get_status_code()) {
        return Ok(response.get_json().await?);
    }

    Err(read_error_body(&mut response).await)
}

/// For endpoints whose response body we do not need: 2xx → `Ok(())`.
async fn handle_http_empty(
    response: Result<FlUrlResponse, FlUrlError>,
) -> Result<(), RequestError> {
    let mut response = response?;

    if is_success(response.get_status_code()) {
        return Ok(());
    }

    Err(read_error_body(&mut response).await)
}

pub async fn get_server_settings() -> Result<ServerSettings, RequestError> {
    let response = FlUrl::new("/api/Settings").get().await;

    handle_http_response(response).await
}

/// Opens (`true`) or closes (`false`) the 10-minute MCP write window of the
/// database mounted on `path`. Windows are per database — this never touches
/// the others.
///
/// The response body echoes the new settings, but callers are expected to
/// re-read via [`get_server_settings`] instead: the server owns the window and
/// its clock, so the authoritative countdown comes from a fresh read.
pub async fn set_mcp_writes(path: String, enabled: bool) -> Result<(), RequestError> {
    let body = SetMcpWritesRequest { path, enabled };

    let response = FlUrl::new("/api/Settings/McpWrites")
        .post(HttpRequestBody::as_json(&body))
        .await;

    handle_http_empty(response).await
}

/// The last statistics the server's background collector published for every
/// database.
///
/// A pure cache read on the server side — polling this does not query Postgres.
pub async fn get_server_stats() -> Result<ServerStats, RequestError> {
    let response = FlUrl::new("/api/Stats").get().await;

    handle_http_response(response).await
}

/// Turns `track_io_timing` on or off on the Postgres server behind `path`.
///
/// A `Ok(result)` with `ok: false` is the normal way a refusal arrives — Postgres
/// declining (not superuser, or the statement not being allowed here) is an answer
/// worth showing, not a failed request.
pub async fn set_track_io_timing(
    path: &str,
    enabled: bool,
) -> Result<TrackIoTimingResult, RequestError> {
    let body = SetTrackIoTimingRequest {
        path: path.to_string(),
        enabled,
    };

    let response = FlUrl::new("/api/Settings/TrackIoTiming")
        .post(HttpRequestBody::as_json(&body))
        .await;

    handle_http_response(response).await
}

/// The recorded 5-second load samples of one database, oldest first.
///
/// One request per database: the history is keyed per mount, and each database gets
/// its own panel rather than being added into a server total — see `LoadCharts`.
pub async fn get_load_history(path: &str, hours: i64) -> Result<LoadHistory, RequestError> {
    // FlUrl builds the query string, so a path like "/mcp-reporting" is escaped
    // rather than concatenated into the URL by hand.
    let response = FlUrl::new("/api/Stats/History")
        .append_query_param("path", Some(path.to_string()))
        .append_query_param("hours", Some(hours.to_string()))
        .append_query_param("section", Some("load".to_string()))
        .get()
        .await;

    handle_http_response(response).await
}

/// The last 100 executed SQL requests, newest first.
pub async fn get_requests() -> Result<Vec<SqlRequestModel>, RequestError> {
    let response = FlUrl::new("/api/Requests").get().await;

    let payload: SqlRequestsResponse = handle_http_response(response).await?;

    Ok(payload.items)
}
