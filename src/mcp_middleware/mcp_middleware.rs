use std::sync::Arc;

use my_http_server::{hyper::Method, *};
use rust_extensions::date_time::DateTimeAsMicroseconds;
use serde::{Serialize, de::DeserializeOwned};

use crate::mcp_middleware::{
    McpInputPayload, McpService, McpSessions, McpToolCalls, SESSION_HEADER, ToolCallExecutor,
};

use my_ai_agent::{ToolDefinition, json_schema::*};

pub struct McpMiddleware {
    mpc_path: &'static str,
    name: &'static str,
    version: &'static str,
    instructions: &'static str,
    sessions: McpSessions,
    tool_calls: McpToolCalls,
}

impl McpMiddleware {
    pub fn new(
        mpc_path: &'static str,
        name: &'static str,
        version: &'static str,
        instructions: &'static str,
    ) -> Self {
        Self {
            mpc_path,
            name,
            version,
            instructions,
            sessions: McpSessions::new(),
            tool_calls: McpToolCalls::new(),
        }
    }

    pub async fn register_tool_call<
        InputData: JsonTypeDescription + Sized + Send + Sync + 'static + Serialize + DeserializeOwned,
        OutputData: JsonTypeDescription + Sized + Send + Sync + 'static + Serialize + DeserializeOwned,
        TMcpService: McpService<InputData, OutputData> + Send + Sync + 'static + ToolDefinition,
    >(
        &mut self,
        service: Arc<TMcpService>,
    ) {
        let executor: ToolCallExecutor<InputData, OutputData> = ToolCallExecutor {
            fn_name: TMcpService::FUNC_NAME,
            description: TMcpService::DESCRIPTION,
            //  input_params: InputData::get_description(false).await,
            //  output_params: OutputData::get_description(false).await,
            holder: service,
        };

        self.tool_calls.add(Arc::new(executor));
    }

    async fn handle_authorized_request(
        &self,
        session_id: &str,
        payload: McpInputPayload,
        now: DateTimeAsMicroseconds,
        id: i64,
    ) -> Result<HttpOkResult, HttpFailResult> {
        match payload.data {
            super::McpInputData::Initialize(contract) => {
                println!("Initializing {:?}", contract);

                let response = super::mcp_output_contract::compile_init_response(
                    &self.name,
                    &self.version,
                    &self.instructions,
                    &contract.protocol_version,
                    id,
                );

                let session_id = self
                    .sessions
                    .generate_session(contract.protocol_version, now)
                    .await;

                return send_response_as_stream(response, session_id.as_str(), now);
            }

            super::McpInputData::ResourcesList => {
                return HttpOutput::Empty.into_ok_result(false);
            }

            super::McpInputData::Ping => {
                let response = super::mcp_output_contract::build_ping_response(id);
                return send_response_as_stream(response, session_id, now);
            }

            super::McpInputData::ExecuteToolCall(params) => {
                let arguments = serde_json::to_string(&params.arguments).unwrap();

                match self.tool_calls.execute(&params.name, &arguments).await {
                    Ok(response) => {
                        let response =
                            super::mcp_output_contract::compile_execute_tool_call_response(
                                response, id,
                            );
                        return send_response_as_stream(response, session_id, now);
                    }
                    Err(err) => {
                        panic!(
                            "Error executing {} with params {}. Err: {}",
                            params.name, arguments, err
                        );
                    }
                }
            }

            super::McpInputData::ToolsList => {
                let list = self.tool_calls.get_list().await;
                let response = super::mcp_output_contract::compile_tool_calls(list, id);

                return send_response_as_stream(response, session_id, now);
            }

            super::McpInputData::NotificationsInitialize => {
                println!("Sending notifications Initialize");
                return HttpOutput::from_builder()
                    .add_header("date", now.to_rfc7231())
                    .set_status_code(202)
                    .into_ok_result(false);
            }
            super::McpInputData::Other { method, data } => {
                println!("Unsupported method: {}. Data: `{}`", method, data);
                return HttpOutput::as_fatal_error(format!(
                    "Unsupported method: {}. data: {}",
                    method, data
                ))
                .into_err(false, false);
            }
        }
    }

    async fn handle_post_request(
        &self,
        session_id: Option<&str>,
        body: &[u8],
    ) -> Result<HttpOkResult, HttpFailResult> {
        println!("Parsing: {}", std::str::from_utf8(body).unwrap());

        let payload = match super::McpInputPayload::try_parse(body) {
            Ok(payload) => payload,
            Err(err) => {
                return HttpOutput::as_fatal_error(format!(
                    "Can not execute http request. Err: {}",
                    err
                ))
                .into_err(false, false);
            }
        };

        let id = payload.id;

        let now = DateTimeAsMicroseconds::now();

        if let Some(session_id) = session_id {
            return self
                .handle_authorized_request(session_id, payload, now, id)
                .await;
        }

        match payload.data {
            super::McpInputData::Initialize(contract) => {
                println!("Initializing {:?}", contract);

                let response = super::mcp_output_contract::compile_init_response(
                    &self.name,
                    &self.version,
                    &self.instructions,
                    &contract.protocol_version,
                    id,
                );

                let session_id = self
                    .sessions
                    .generate_session(contract.protocol_version, now)
                    .await;

                send_response_as_stream(response, session_id.as_str(), now)
            }
            super::McpInputData::Other { method, data } => {
                println!("Unsupported method: {}. Data: `{}`", method, data);
                return HttpOutput::as_fatal_error(format!(
                    "Unsupported method: {}. data: {}",
                    method, data
                ))
                .into_err(false, false);
            }

            _ => HttpOutput::as_unauthorized(None).into_err(false, false),
        }
    }
}

/*
fn send_response(response: String) -> Result<HttpOkResult, HttpFailResult> {
    HttpOutput::Content {
        status_code: 200,
        headers: None,
        content_type: Some(WebContentType::Json),
        set_cookies: None,
        content: response.into_bytes(),
    }
    .into_ok_result(false)
}
     */
fn send_response_as_stream(
    response: String,
    session_id: &str,
    now: DateTimeAsMicroseconds,
) -> Result<HttpOkResult, HttpFailResult> {
    let (http_output, mut producer) = HttpOutput::as_stream(1024);
    tokio::spawn(async move {
        println!("Sending response: `{}`", response);
        let payload = response.into_bytes();
        producer.send(payload).await.unwrap();
    });

    http_output
        .with_header(SESSION_HEADER, session_id)
        .with_header("cache-control", "no-cache")
        .with_header("content-type", "text/event-stream")
        .with_header("date", now.to_rfc7231())
        .get_result()
}

#[async_trait::async_trait]
impl HttpServerMiddleware for McpMiddleware {
    async fn handle_request(
        &self,
        ctx: &mut HttpContext,
    ) -> Option<Result<HttpOkResult, HttpFailResult>> {
        if !ctx
            .request
            .get_path()
            .equals_to_case_insensitive(self.mpc_path)
        {
            return None;
        }

        println!(
            "Mpc Middleware {:?} {}",
            ctx.request.method,
            ctx.request.get_path().as_str()
        );

        let session_id = ctx
            .request
            .get_headers()
            .try_get_case_sensitive(SESSION_HEADER)
            .map(|itm| itm.as_str().unwrap().to_string());

        match ctx.request.method {
            Method::GET => {
                let Some(session_id) = session_id else {
                    return Some(
                        HttpFailResult::as_unauthorized(Some("Unauthorized request")).into_err(),
                    );
                };

                if let Some(receiver) = self
                    .sessions
                    .subscribe_to_notifications(session_id.as_str())
                    .await
                {
                    let (stream, producer) = HttpOutput::as_stream(32);
                    tokio::spawn(super::stream_updates(producer, receiver));

                    let now = DateTimeAsMicroseconds::now();
                    return Some(stream.with_header("date", now.to_rfc7231()).get_result());
                }

                return Some(
                    HttpFailResult::as_unauthorized(Some("Unauthorized request")).into_err(),
                );
            }
            Method::POST => {
                let body = match ctx.request.get_body().await {
                    Ok(body) => body,
                    Err(err) => {
                        return Some(Err(err));
                    }
                };

                let result = self
                    .handle_post_request(session_id.as_deref(), body.as_slice())
                    .await;
                return Some(result);
            }
            Method::DELETE => {}
            _ => {}
        }

        None
    }
}
