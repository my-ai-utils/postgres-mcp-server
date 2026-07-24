use std::sync::Arc;

use my_ai_agent::{ToolDefinition, macros::ApplyJsonSchema};
use rust_extensions::date_time::DateTimeAsMicroseconds;
use serde::*;

use crate::app::AppContext;
use crate::sql_guard::SqlKind;
use crate::sql_log::SqlRequestStatus;
use mcp_server_middleware::McpToolCall;

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct SqlRequestToolCallRequest {
    #[property( description: "Sql request to execute")]
    pub sql_request: String,
}

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct SqlRequestToolCallResponse {
    #[property( description: "Sql response as json")]
    pub sql_response_as_json: String,
}

pub struct PostgresMcpService {
    app: Arc<AppContext>,
}

impl PostgresMcpService {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

impl ToolDefinition for PostgresMcpService {
    const FUNC_NAME: &'static str = "sql_request";
    const DESCRIPTION: &'static str = "Execute a sql request to database described in system prompt and return the result as json. Read-only statements (SELECT and friends) always work. Statements that write — INSERT, UPDATE, DELETE, TRUNCATE, DDL and anything else that is not a plain read — are refused unless the user has granted write access from the server UI; see the 'write_access_policy' prompt.";
}

#[async_trait::async_trait]
impl McpToolCall<SqlRequestToolCallRequest, SqlRequestToolCallResponse> for PostgresMcpService {
    async fn execute_tool_call(
        &self,
        model: SqlRequestToolCallRequest,
    ) -> Result<SqlRequestToolCallResponse, String> {
        let sql = model.sql_request;
        let started = DateTimeAsMicroseconds::now();

        let kind = crate::sql_guard::classify(&sql);

        // Refuse before touching Postgres, but still log the attempt — a refusal
        // the user cannot see looks like the tool silently did nothing.
        if let SqlKind::Write { reason } = &kind {
            if let Err(message) = crate::sql_guard::ensure_mcp_writes_enabled(&self.app, reason) {
                self.app.sql_log.add(
                    sql,
                    true,
                    started,
                    None,
                    SqlRequestStatus::Blocked {
                        message: message.clone(),
                    },
                );
                return Err(message);
            }
        }

        let result = self.app.postgres.do_request(sql.clone()).await;

        let took_micros = DateTimeAsMicroseconds::now()
            .duration_since(started)
            .get_full_micros()
            .max(0) as u64;

        match result {
            Ok(response) => {
                self.app.sql_log.add(
                    sql,
                    kind.is_write(),
                    started,
                    Some(took_micros),
                    SqlRequestStatus::Ok {
                        rows: response.rows,
                    },
                );

                Ok(SqlRequestToolCallResponse {
                    sql_response_as_json: response.json,
                })
            }
            Err(err) => {
                self.app.sql_log.add(
                    sql,
                    kind.is_write(),
                    started,
                    Some(took_micros),
                    SqlRequestStatus::Error {
                        message: err.clone(),
                    },
                );

                Err(err)
            }
        }
    }
}
