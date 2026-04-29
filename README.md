# postgres-mcp-server

MCP (Model Context Protocol) server that exposes a PostgreSQL database as a single tool call. An LLM client (Claude Desktop, Claude Code, Cursor, any MCP-compatible agent) connects to this server, sends arbitrary SQL, and gets the result back as JSON.

## What it does

- Listens on HTTP `:8005` and serves the MCP protocol at `POST /mcp`.
- Exposes one tool: **`sql_request`** — accepts a `sql_request: string`, runs it against the configured Postgres, returns rows as JSON.

### Response format

Columnar (compact, friendly to LLM token budgets):

```json
{
  "columns": ["id", "email"],
  "rows": [
    [1, "a@b.com"],
    [2, "c@d.com"]
  ]
}
```

Column types are detected at runtime in this order: `i8 → i16 → i32 → i64 → f32 → f64 → bool → String`. Any column type not in that list will currently fail (see [Limitations](#limitations)).

## Configuration

Settings are read from `~/.postgres-mcp-server` (JSON5):

```json5
{
  postgres_url: "host=localhost user=postgres password=secret dbname=mydb"
}
```

The connection string is the standard `tokio_postgres` form. SSH tunneling and TLS features are compiled in via `my-postgres`.

## Running

### From source

```sh
cargo run --release
```

### Docker

```sh
cargo build --release
docker build -t postgres-mcp-server .
docker run --rm -p 8005:8005 -v ~/.postgres-mcp-server:/root/.postgres-mcp-server postgres-mcp-server
```

## Connecting an MCP client

Point your MCP client at `http://localhost:8005/mcp`. Example for Claude Desktop's `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "postgres": {
      "url": "http://localhost:8005/mcp"
    }
  }
}
```

The session is established automatically on first request (`initialize`); subsequent calls reuse the `mcp-session-id` header.

## How a request flows

1. Client → `POST /mcp` with `tools/call` for `sql_request`.
2. [`PostgresMcpService::execute_tool_call`](src/mcp_service/sql_request.rs) receives the SQL string.
3. [`PostgresAccess::do_request`](src/mcp/mcp.rs) executes it via `MyPostgres::execute_sql_as_vec` with a 10s timeout.
4. Each row is converted to a JSON value array; column names are captured once.
5. Result is wrapped as `{ "sql_response_as_json": "<json>" }` and returned through the MCP middleware.

## Limitations

- **No parameterized queries** — the SQL string is executed as-is. Do not expose this server to untrusted clients.
- **Limited type coverage** — `NULL`, `uuid`, `timestamp`, `numeric`, `bytea`, `json`/`jsonb`, arrays will panic on `row.get::<String>` because no matching `try_get` branch handles them.
- **10s query timeout** is hardcoded.
- **Single database** per server instance.

## Project layout

```
src/
├── main.rs              # bootstrap
├── settings.rs          # ~/.postgres-mcp-server reader
├── app/                 # AppContext (postgres handle, app state)
├── http_server/         # MyHttpServer + MCP middleware wiring
├── mcp_service/         # sql_request tool definition
└── postgres/            # SQL execution + row → JSON conversion
```

## Dependencies

- [`mcp-server-middleware`](https://github.com/my-ai-utils/mcp-server-middleware) — MCP protocol handling
- [`my-http-server`](https://github.com/MyJetTools/my-http-server) — HTTP runtime
- [`my-postgres`](https://github.com/MyJetTools/my-postgres) — Postgres client
- [`my-ai-agent`](https://github.com/my-ai-utils/my-ai-agent) — JSON schema derivation for tool I/O
