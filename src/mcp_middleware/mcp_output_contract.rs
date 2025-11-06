use super::*;
use my_ai_agent::my_json::{
    self,
    json_writer::{JsonObjectWriter, RawJsonObject},
};

pub fn compile_init_response(
    name: &str,
    version: &str,
    instructions: &str,
    protocol_version: &str,
    id: i64,
) -> String {
    let json_builder =
        my_json::json_writer::JsonObjectWriter::new().write_json_object("result", |result| {
            result
                .write("protocolVersion", protocol_version)
                .write_json_object("capabilities", |cap| {
                    cap.write_json_object("resources", |res| res.write("listChanged", true))
                        .write_json_object("tools", |res| res.write("listChanged", true))
                })
                .write_json_object("serverInfo", |server_info| {
                    server_info.write("name", name).write("version", version)
                })
                .write("instructions", instructions)
        });

    build(json_builder, id)
}

pub fn compile_tool_calls(tools: Vec<ToolCallSchemaData>, id: i64) -> String {
    let json_builder = JsonObjectWriter::new().write_json_object("result", |result| {
        result.write_json_array("tools", |mut arr| {
            for tool in tools.iter() {
                arr = arr.write_json_object(|obj| {
                    obj.write("name", tool.mcp.get_fn_name())
                        .write("description", tool.mcp.get_description())
                        .write_ref("inputSchema", &tool.input)
                        .write_ref("outputSchema", &tool.output)
                });
            }

            arr
        })
    });

    build(json_builder, id)
}

pub fn compile_execute_tool_call_response(response: String, id: i64) -> String {
    let mut result = JsonObjectWriter::new()
        .write("jsonrpc", "2.0")
        .write("id", id)
        .write_json_object("result", |result| {
            result
                .write_json_array("content", |arr| {
                    arr.write_json_object(|obj| {
                        obj.write("type", "text").write("text", response.as_str())
                    })
                })
                .write("structuredContent", RawJsonObject::AsStr(&response))
                .write("isError", false)
        })
        .build();

    result.push('\n');
    result.push('\n');

    result.insert_str(0, "data: ");
    result
}

pub fn build_ping_response(id: i64) -> String {
    let mut result = JsonObjectWriter::new()
        .write("jsonrpc", "2.0")
        .write("id", id)
        .write_json_object("result", |o| o)
        .build();

    result.insert_str(0, "data: ");
    result.push('\n');
    result.push('\n');

    result
}

fn build(json: JsonObjectWriter, id: i64) -> String {
    let mut result = "data: ".to_string();
    json.write("jsonrpc", "2.0")
        .write("id", id)
        .build_into(&mut result);

    result.push('\n');
    result.push('\n');
    result
}
