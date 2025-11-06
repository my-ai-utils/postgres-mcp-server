use std::sync::Arc;

use my_ai_agent::{ToolDefinition, macros::ApplyJsonSchema};
use serde::*;

use crate::{app::AppContext, mcp_middleware::McpService};

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
    const DESCRIPTION: &'static str = "Execute a sql request to database described in system prompt and return the result as json";
}

#[async_trait::async_trait]
impl McpService<SqlRequestToolCallRequest, SqlRequestToolCallResponse> for PostgresMcpService {
    async fn execute_tool_call(
        &self,
        model: SqlRequestToolCallRequest,
    ) -> Result<SqlRequestToolCallResponse, String> {
        println!("Executing Mcp with params: {:?}", model);

        let response = self.app.postgres.do_request(model.sql_request).await;

        Ok(SqlRequestToolCallResponse {
            sql_response_as_json: response,
        })
    }
}
