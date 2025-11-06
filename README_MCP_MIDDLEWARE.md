# MCP Server Middleware

A Rust middleware library for implementing Model Context Protocol (MCP) servers. This middleware handles MCP protocol communication, session management, and tool call execution, making it easy to build MCP-compatible servers.

## Features

- **MCP Protocol Support**: Full implementation of MCP protocol including initialization, tool calls, and notifications
- **Session Management**: Automatic session creation and management with session-based authentication
- **Tool Call Framework**: Easy-to-use trait-based system for implementing custom tool calls
- **HTTP Integration**: Seamless integration with `my-http-server` as middleware
- **Type-Safe Tool Definitions**: Leverages `my-ai-agent` for type-safe JSON schema generation

## Installation

Add the dependency to your `Cargo.toml`:

```toml
[dependencies]
mcp-server-middleware = { git = "https://github.com/my-ai-utils/mcp-server-middleware.git" }
my-http-server = { tag = "0.8.3", git = "https://github.com/MyJetTools/my-http-server.git", features = ["macros", "controllers"] }
my-ai-agent = { tag = "0.1.0", git = "https://github.com/my-ai-utils/my-ai-agent.git", features = ["agent"] }
tokio = { version = "1", features = ["full"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
async-trait = "*"
```

## Quick Start

### 1. Create the Middleware

Create an instance of `McpMiddleware` with your server configuration:

```rust
use mcp_server_middleware::McpMiddleware;
use std::sync::Arc;

let mut mcp_middleware = McpMiddleware::new(
    "/postgres",                    // MCP endpoint path
    "Postgres MCP Server",          // Server name
    "0.1.0",                        // Server version
    "You can use this server to query your Postgres database", // Instructions
);
```

### 2. Implement a Tool Service

Create a service that implements the `McpService` trait:

```rust
use mcp_server_middleware::{McpService, ToolDefinition};
use my_ai_agent::{macros::ApplyJsonSchema, json_schema::*};
use serde::{Deserialize, Serialize};
use async_trait::async_trait;

// Define your input and output types with JSON schema
#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct SqlRequestToolCallRequest {
    #[property(description: "SQL request to execute")]
    pub sql_request: String,
}

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct SqlRequestToolCallResponse {
    #[property(description: "SQL response as JSON")]
    pub sql_response_as_json: String,
}

// Implement ToolDefinition to provide metadata
impl ToolDefinition for PostgresMcpService {
    const FUNC_NAME: &'static str = "sql_request";
    const DESCRIPTION: &'static str = "Execute a SQL request to database and return the result as JSON";
}

// Implement McpService to handle tool execution
#[async_trait]
impl McpService<SqlRequestToolCallRequest, SqlRequestToolCallResponse> for PostgresMcpService {
    async fn execute_tool_call(
        &self,
        model: SqlRequestToolCallRequest,
    ) -> Result<SqlRequestToolCallResponse, String> {
        // Your implementation here
        let response = self.execute_sql(model.sql_request).await?;
        
        Ok(SqlRequestToolCallResponse {
            sql_response_as_json: response,
        })
    }
}
```

### 3. Register Tool Calls

Register your service with the middleware:

```rust
let service = Arc::new(PostgresMcpService::new(app_context));
mcp_middleware.register_tool_call(service).await;
```

### 4. Integrate with HTTP Server

Add the middleware to your HTTP server:

```rust
use my_http_server::MyHttpServer;
use std::net::SocketAddr;

let mut http_server = MyHttpServer::new(SocketAddr::from(([0, 0, 0, 0], 8005)));
let mcp_middleware = Arc::new(mcp_middleware);
http_server.add_middleware(mcp_middleware);
http_server.start(app_states, logger);
```

## Complete Example

Here's a complete example based on the postgres-mcp-server implementation:

```rust
use std::sync::Arc;
use mcp_server_middleware::{McpMiddleware, McpService, ToolDefinition};
use my_http_server::MyHttpServer;
use my_ai_agent::{macros::ApplyJsonSchema, json_schema::*};
use serde::{Deserialize, Serialize};
use async_trait::async_trait;

// Define your service
pub struct PostgresMcpService {
    // Your service dependencies
}

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct SqlRequest {
    #[property(description: "SQL query to execute")]
    pub sql: String,
}

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct SqlResponse {
    #[property(description: "Query result as JSON")]
    pub result: String,
}

impl ToolDefinition for PostgresMcpService {
    const FUNC_NAME: &'static str = "sql_request";
    const DESCRIPTION: &'static str = "Execute SQL queries";
}

#[async_trait]
impl McpService<SqlRequest, SqlResponse> for PostgresMcpService {
    async fn execute_tool_call(
        &self,
        model: SqlRequest,
    ) -> Result<SqlResponse, String> {
        // Execute your SQL query
        let result = execute_query(&model.sql).await?;
        Ok(SqlResponse { result })
    }
}

// Setup function
async fn setup_server() {
    let mut http_server = MyHttpServer::new(SocketAddr::from(([0, 0, 0, 0], 8005)));
    
    // Create middleware
    let mut mcp_middleware = McpMiddleware::new(
        "/postgres",
        "Postgres MCP Server",
        "0.1.0",
        "Execute SQL queries on your database",
    );
    
    // Register tool
    let service = Arc::new(PostgresMcpService::new());
    mcp_middleware.register_tool_call(service).await;
    
    // Add to server
    let mcp_middleware = Arc::new(mcp_middleware);
    http_server.add_middleware(mcp_middleware);
    
    // Start server
    http_server.start(app_states, logger);
}
```

## API Reference

### `McpMiddleware`

The main middleware struct that handles MCP protocol communication.

#### `new(path, name, version, instructions)`

Creates a new middleware instance.

- `path`: The HTTP path where MCP requests will be handled (e.g., `/postgres`)
- `name`: Server name displayed to clients
- `version`: Server version string
- `instructions`: Instructions for the AI agent using this server

#### `register_tool_call(service)`

Registers a tool call service. The service must implement:
- `McpService<InputData, OutputData>` trait
- `ToolDefinition` trait
- Input and output types must implement `JsonTypeDescription`, `Serialize`, and `DeserializeOwned`

### `McpService` Trait

Trait that must be implemented by your tool services:

```rust
#[async_trait]
pub trait McpService<InputData, OutputData>
where
    InputData: JsonTypeDescription + Sized + Send + Sync + 'static,
    OutputData: JsonTypeDescription + Sized + Send + Sync + 'static,
{
    async fn execute_tool_call(&self, model: InputData) -> Result<OutputData, String>;
}
```

### `ToolDefinition` Trait

Provides metadata about your tool:

```rust
pub trait ToolDefinition {
    const FUNC_NAME: &'static str;
    const DESCRIPTION: &'static str;
}
```

## MCP Protocol Support

The middleware handles the following MCP protocol methods:

- **`initialize`**: Initializes a new MCP session and returns server capabilities
- **`tools/list`**: Returns a list of available tools with their schemas
- **`tools/call`**: Executes a tool call with the provided arguments
- **`ping`**: Health check endpoint
- **`resources/list`**: Returns available resources (currently returns empty)
- **`notifications/initialized`**: Handles initialization notifications

## Session Management

Sessions are automatically managed by the middleware:

- Each `initialize` request creates a new session with a unique session ID
- Session IDs are returned in the `mcp-session-id` HTTP header
- Subsequent requests must include the session ID in the `mcp-session-id` header
- GET requests to the MCP path establish Server-Sent Events (SSE) streams for notifications

## Type Safety

The middleware leverages `my-ai-agent`'s `ApplyJsonSchema` macro to automatically generate JSON schemas for your input and output types. Use the `#[property(description = "...")]` attribute to document your fields:

```rust
#[derive(ApplyJsonSchema, Serialize, Deserialize)]
pub struct MyRequest {
    #[property(description: "A description of this field")]
    pub field: String,
}
```

## Error Handling

Tool execution errors should be returned as `Err(String)` from `execute_tool_call`. The middleware will format these appropriately in the MCP response.

## Dependencies

- `my-http-server`: HTTP server framework
- `my-ai-agent`: AI agent utilities and JSON schema generation
- `tokio`: Async runtime
- `serde` / `serde_json`: Serialization
- `async-trait`: Async trait support

## License

[Add your license here]

## Contributing

[Add contribution guidelines here]

