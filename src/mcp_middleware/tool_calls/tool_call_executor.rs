use std::sync::Arc;

use my_ai_agent::{json_schema::*, my_json};
use serde::{Serialize, de::DeserializeOwned};

use crate::mcp_middleware::{McpService, McpServiceAbstract};

pub struct ToolCallExecutor<InputData, OutputData>
where
    InputData: JsonTypeDescription + Sized + Send + Sync + 'static,
    OutputData: JsonTypeDescription + Sized + Send + Sync + 'static,
{
    pub fn_name: &'static str,
    pub description: &'static str,
    //pub input_params: my_json::json_writer::JsonObjectWriter,
    //pub output_params: my_json::json_writer::JsonObjectWriter,
    pub holder: Arc<dyn McpService<InputData, OutputData> + Send + Sync + 'static>,
}

#[async_trait::async_trait]
impl<InputData, OutputData> McpServiceAbstract for ToolCallExecutor<InputData, OutputData>
where
    InputData: JsonTypeDescription + Sized + Send + Sync + 'static + Serialize + DeserializeOwned,
    OutputData: JsonTypeDescription + Sized + Send + Sync + 'static + Serialize + DeserializeOwned,
{
    fn get_fn_name(&self) -> &str {
        &self.fn_name
    }

    fn get_description(&self) -> &str {
        &self.description
    }

    async fn get_input_params(&self) -> my_json::json_writer::JsonObjectWriter {
        InputData::get_description(false, None, false).await
    }

    async fn get_output_params(&self) -> my_json::json_writer::JsonObjectWriter {
        OutputData::get_description(false, None, true).await
    }

    async fn execute(&self, input: &str) -> Result<String, String> {
        let parse_result: Result<InputData, serde_json::Error> = serde_json::from_str(input);

        let result = match parse_result {
            Ok(input) => self.holder.execute_tool_call(input).await?,
            Err(err) => {
                let msg = format!("Can not deserialize input data {}. Msg: {:?}", input, err);
                println!("{}", msg);
                return Err(msg);
            }
        };

        Ok(serde_json::to_string(&result).unwrap())
    }
}
