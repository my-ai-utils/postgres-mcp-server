use my_ai_agent::my_json;

use super::*;
use std::{collections::BTreeMap, sync::Arc};

pub struct ToolCallSchemaData {
    pub mcp: Arc<dyn McpServiceAbstract + Send + Sync + 'static>,
    pub input: my_json::json_writer::JsonObjectWriter,
    pub output: my_json::json_writer::JsonObjectWriter,
}

pub struct McpToolCalls {
    tool_calls: BTreeMap<String, Arc<dyn McpServiceAbstract + Send + Sync + 'static>>,
}

impl McpToolCalls {
    pub fn new() -> Self {
        Self {
            tool_calls: BTreeMap::new(),
        }
    }

    pub fn add(&mut self, executor: Arc<dyn McpServiceAbstract + Send + Sync + 'static>) {
        let name = executor.get_fn_name().to_string();
        self.tool_calls.insert(name, executor);
    }

    pub async fn execute(&self, fn_name: &str, input: &str) -> Result<String, String> {
        if let Some(executor) = self.tool_calls.get(fn_name) {
            return executor.execute(input).await;
        }

        Err(format!("Tool call with name {} is not found", fn_name))
    }

    pub async fn get_list(&self) -> Vec<ToolCallSchemaData> {
        let mut result = Vec::with_capacity(self.tool_calls.len());

        for tool_call in self.tool_calls.values() {
            let input = tool_call.get_input_params().await;
            let output = tool_call.get_output_params().await;
            result.push(ToolCallSchemaData {
                mcp: tool_call.clone(),
                input,
                output,
            });
        }

        result
    }
}
