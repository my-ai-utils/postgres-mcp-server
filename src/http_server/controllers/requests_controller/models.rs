use my_http_server::macros::*;
use serde::{Deserialize, Serialize};

use crate::sql_log::{SqlLogItem, SqlRequestStatus};

/// One row of the requests table. Optional fields are omitted rather than
/// nulled, so the UI's `Option` + `#[serde(default)]` mirror deserializes the
/// same whether or not the field applies.
#[derive(Serialize, Deserialize, Debug, Clone, MyHttpObjectStructure)]
pub struct SqlRequestModel {
    pub id: u64,
    pub started: String,
    pub sql: String,
    // "read" | "write". No `///` doc-comments on these fields: the
    // MyHttpObjectStructure proc-macro panics on them.
    pub kind: String,
    // "ok" | "error" | "blocked"
    pub status: String,
    // Rows returned — present only when the request succeeded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<usize>,
    // Absent for gate-blocked requests: they never ran, so they took no time.
    #[serde(rename = "tookMicros", skip_serializing_if = "Option::is_none")]
    pub took_micros: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
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
