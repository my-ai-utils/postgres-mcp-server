use my_http_server::macros::*;
use serde::{Deserialize, Serialize};

use crate::sql_log::{SqlLogItem, SqlRequestStatus};

/// One row of the requests table. Optional fields go on the wire as `null` when
/// they do not apply — never omitted, because the UI mirror declares them as
/// plain `Option`s and would fail on an absent key.
#[derive(Serialize, Deserialize, Debug, Clone, MyHttpObjectStructure)]
pub struct SqlRequestModel {
    pub id: u64,
    // Mount path of the database the request ran against, e.g. "/mcp".
    pub db: String,
    pub started: String,
    pub sql: String,
    // "read" | "write". No `///` doc-comments on these fields: the
    // MyHttpObjectStructure proc-macro panics on them.
    pub kind: String,
    // "ok" | "error" | "blocked"
    pub status: String,
    // Rows returned — non-null only when the request succeeded.
    pub rows: Option<usize>,
    // Null for gate-blocked requests: they never ran, so they took no time.
    #[serde(rename = "tookMicros")]
    pub took_micros: Option<u64>,
    pub error: Option<String>,
}

impl SqlRequestModel {
    pub fn new(src: &SqlLogItem) -> Self {
        let (rows, error) = match &src.status {
            SqlRequestStatus::Ok { rows } => (Some(*rows), None),
            SqlRequestStatus::Error { message } => (None, Some(message.clone())),
            SqlRequestStatus::Blocked { message } => (None, Some(message.clone())),
        };

        Self {
            id: src.id,
            db: src.db_path.as_str().to_string(),
            started: src.started.to_rfc3339(),
            sql: src.sql.clone(),
            kind: if src.is_write { "write" } else { "read" }.to_string(),
            status: src.status.as_str().to_string(),
            rows,
            took_micros: src.took_micros,
            error,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, MyHttpObjectStructure)]
pub struct SqlRequestsModel {
    pub items: Vec<SqlRequestModel>,
}
