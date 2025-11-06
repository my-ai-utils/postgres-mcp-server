use std::{net::SocketAddr, sync::Arc};

use mcp_server_middleware::McpMiddleware;
use my_http_server::{HttpConnectionsCounter, MyHttpServer, StaticFilesMiddleware};

use crate::{app::AppContext, mcp_service::PostgresMcpService};

pub async fn setup_server(app: &Arc<AppContext>) -> HttpConnectionsCounter {
    let mut http_server = MyHttpServer::new(SocketAddr::from(([0, 0, 0, 0], 8005)));

    let mut mcp_middleware = McpMiddleware::new(
        "/postgres",
        "Postgres MCP Server",
        "0.1.0",
        "You can use this server to query your Postgres database",
    );
    mcp_middleware
        .register_tool_call(Arc::new(PostgresMcpService::new(app.clone())))
        .await;

    let mcp_middleware = Arc::new(mcp_middleware);

    http_server.add_middleware(mcp_middleware);

    http_server.add_middleware(Arc::new(StaticFilesMiddleware::new(None, None)));

    http_server.start(app.app_states.clone(), my_logger::LOGGER.clone());

    http_server.get_http_connections_counter()
}
