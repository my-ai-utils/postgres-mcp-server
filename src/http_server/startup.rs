use std::{net::SocketAddr, sync::Arc};

use mcp_server_middleware::McpMiddleware;
use my_http_server::controllers::swagger::SwaggerMiddleware;
use my_http_server::{HttpConnectionsCounter, MyHttpServer, StaticFilesMiddleware};

use crate::{
    app::{APP_VERSION, AppContext, DbContext},
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

    // Order is load-bearing: the static-files fallback answers every unmatched
    // path, so it has to sit last or it would swallow /api, /swagger and the MCP
    // endpoints.
    http_server.add_middleware(controllers);
    http_server.add_middleware(swagger_middleware);

    // One MCP middleware per configured database. Each matches its own exact
    // path and passes everything else down the chain, so the endpoints never see
    // each other's traffic — which is the whole isolation story: an agent
    // connected to one path cannot reach another database, because the tool it
    // is calling is bound to a single `DbContext`. Nothing on the MCP surface
    // says the other mounts exist; the list of databases lives in the admin UI
    // only.
    for db in &app.databases {
        http_server.add_middleware(build_mcp_middleware(app, db));
    }

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

fn build_mcp_middleware(app: &Arc<AppContext>, db: &Arc<DbContext>) -> Arc<McpMiddleware> {
    let mut mcp_middleware = McpMiddleware::new(
        leak(db.path.as_str().to_string()),
        leak(format!("Postgres MCP Server ({})", db.description)),
        APP_VERSION,
        leak(build_instructions(db)),
    );

    mcp_middleware.register_tool_call(Arc::new(PostgresMcpService::new(
        app.clone(),
        db.clone(),
    )));
    mcp_middleware.register_prompt(Arc::new(WriteAccessPolicyPromptHandler::new(db.clone())));

    Arc::new(mcp_middleware)
}

/// What the client is told about this endpoint on `initialize`.
///
/// An endpoint describes **its own** database and nothing else. The other mounts
/// are not mentioned, not counted and not listed anywhere on the MCP surface:
/// each path is handed to a different project, and from inside one of them this
/// is a server with exactly one database.
fn build_instructions(db: &Arc<DbContext>) -> String {
    format!(
        "You can use this server to query your Postgres database: {}. Writes are refused unless \
         the user has granted write access in the server UI — see the 'write_access_policy' \
         prompt.",
        db.description
    )
}

/// `McpMiddleware` takes `&'static str`s, but the paths and instructions are
/// only known once the settings file has been read. Leaking is the honest fix:
/// it happens once per configured database at startup, and those strings have to
/// live as long as the process anyway.
fn leak(value: String) -> &'static str {
    Box::leak(value.into_boxed_str())
}
