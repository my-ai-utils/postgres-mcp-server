use my_ai_agent::{json_schema::*, my_json};

#[async_trait::async_trait]
pub trait McpService<InputData, OutputData>
where
    InputData: JsonTypeDescription + Sized + Send + Sync + 'static,
    OutputData: JsonTypeDescription + Sized + Send + Sync + 'static,
{
    async fn execute_tool_call(&self, model: InputData) -> Result<OutputData, String>;
}

#[async_trait::async_trait]
pub trait McpServiceAbstract {
    async fn execute(&self, input: &str) -> Result<String, String>;

    fn get_fn_name(&self) -> &str;
    fn get_description(&self) -> &str;
    async fn get_input_params(&self) -> my_json::json_writer::JsonObjectWriter;
    async fn get_output_params(&self) -> my_json::json_writer::JsonObjectWriter;
}
