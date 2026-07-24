use std::{net::SocketAddr, sync::Arc};

use mcp_server_middleware::McpMiddleware;
use my_http_server::controllers::swagger::SwaggerMiddleware;
use my_http_server::{HttpConnectionsCounter, MyHttpServer, StaticFilesMiddleware};

use crate::{
    app::{APP_VERSION, AppContext},
    mcp_service::{PostgresMcpService, WriteAccessPolicyPromptHandler},
};

pub async fn setup_server(app: &Arc<AppContext>) -> HttpConnectionsCounter {
    let mut http_server = MyHttpServer::new(SocketAddr::from(([0, 0, 0, 0], 8000)));

    let controllers = Arc::new(crate::http_server::controllers::builder::build(app));

    let swagger_middleware = Arc::new(SwaggerMiddleware::new(
        controllers.clone(),
        "Postgres MCP Server".to_string(),
        APP_VERSION.to_string(),
    ));

    let mut mcp_middleware = McpMiddleware::new(
        "/mcp",
        "Postgres MCP Server",
        APP_VERSION,
        "You can use this server to query your Postgres database",
    );
    mcp_middleware
        .register_tool_call(Arc::new(PostgresMcpService::new(app.clone())))
        .await;
    mcp_middleware
        .register_prompt(Arc::new(WriteAccessPolicyPromptHandler))
        .await;

    let mcp_middleware = Arc::new(mcp_middleware);

    // Order is load-bearing: the static-files fallback answers every unmatched
    // path, so it has to sit last or it would swallow /api, /swagger and /mcp.
    http_server.add_middleware(controllers);
    http_server.add_middleware(swagger_middleware);
    http_server.add_middleware(mcp_middleware);
    http_server.add_middleware(Arc::new(
        StaticFilesMiddleware::new()
            .add_index_file("index.html")
            // The UI is a SPA: a deep link must return index.html, not a 404.
            .set_not_found_file("index.html".to_string())
            // Without ETags a redeployed wwwroot can be served from a stale
            // browser cache; dx content-hashes the wasm/js bundles but not
            // app.css or index.html.
            .with_etag(),
    ));

    http_server.start(app.app_states.clone(), my_logger::LOGGER.clone());

    http_server.get_http_connections_counter()
}
