# postgres-mcp-server

MCP (Model Context Protocol) server that exposes a PostgreSQL database as a single tool call. An LLM client (Claude Desktop, Claude Code, Cursor, any MCP-compatible agent) connects to this server, sends arbitrary SQL, and gets the result back as JSON.

It also serves a small web UI that shows what the agent has been running and gates write access behind an explicit, time-limited grant.

## What it does

- Listens on HTTP `:8000` and serves the MCP protocol at `POST /mcp`.
- Exposes one tool: **`sql_request`** — accepts a `sql_request: string`, runs it against the configured Postgres, returns rows as JSON.
- Exposes one MCP prompt: **`write_access_policy`** — explains the write gate to the agent.
- Serves the admin UI at `/`, Swagger at `/swagger`.

## Write access

Read-only SQL runs at any time. Anything that writes is **refused by default**; the user grants writes from the UI's *Write access* card. Every click adds **10 minutes** on top of whatever is left, so pressing it three times buys 30 minutes; *Disable* resets it to closed. The window auto-expires, and a restart always comes up disabled — it is runtime-only state and is never persisted.

The server **allow-lists reads** rather than deny-listing writes: only `SELECT`, `WITH ... SELECT`, `EXPLAIN` (without `ANALYZE`), `SHOW`, `TABLE` and `VALUES` are treated as reads. Everything else — including anything the classifier cannot parse — needs the window. That direction is deliberate: a deny-list fails *open* (one missed statement is an unguarded write), an allow-list fails *closed* (the worst case is a refused `SELECT`, which the user sees and can fix).

Cases the classifier catches that a keyword scan would not:

| Statement | Why it is a write |
|---|---|
| `WITH x AS (INSERT ... RETURNING *) SELECT * FROM x` | data-modifying CTE — leads with `WITH` |
| `SELECT * INTO t2 FROM t1` | creates a table |
| `EXPLAIN ANALYZE INSERT ...` | `ANALYZE` really executes the statement |
| `SELECT ... FOR UPDATE` | takes row locks |
| `SELECT nextval('s')` | side-effecting function |
| `SET`, `BEGIN`, `DECLARE` | leaks state onto a pooled connection |

On top of that there is a blunt second pass: a statement is refused if its **raw text** contains `insert`, `update`, `delete`, `merge`, `truncate`, `drop`, `alter`, `create`, `grant` or `revoke` as a whole word — **including inside string literals and comments**. So `SELECT * FROM audit WHERE action = 'insert'` needs the window even though it only reads.

That over-blocks by design. The rule is meant to be predictable ("if it says insert, click the button") rather than clever, and it means a bug in the tokenizer cannot turn into an unguarded write. A word must match in full, so `delete_flag`, `updated_users` and `user_insert_log` stay reads.

`END` is deliberately **not** in that list — it would refuse every `CASE ... END`. Neither are `set`, `do`, `call`, `begin`, `refresh`, `vacuum` and friends: they are ordinary English (common in real data) and can only ever be a statement's *leading* keyword, which the allow-list already catches. Scanning for them would add false positives and no safety.

> **The gate is an operator guardrail, not a security boundary.** The MCP and HTTP endpoints are unauthenticated, and no string classifier can know what a called function's body does. The real boundary is a least-privileged Postgres role in the connection string.

## Request log

The last **100** requests are kept in memory (`GET /api/Requests`, newest first) and shown in the UI: time, SQL, rows returned, duration, status. Refusals are logged too — a blocked request that left no trace would look like the tool silently did nothing. The log is in-memory only and empty after a restart.

Note that `rows` means **rows returned**, not rows affected: queries go through the extended protocol, so a write without `RETURNING` reports 0 rows.

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

Types are mapped per Postgres column type (`bool`, ints, floats, text, `uuid`, timestamps, `date`/`time`, `json`/`jsonb`, `bytea` as hex). `NULL` becomes a JSON null. An unmapped type renders as `[unsupported pg type: <name>]`.

## Configuration

Settings are read from `~/.postgres-mcp-server` (JSON5):

```json5
{
  postgres_conn_string: "host=localhost user=postgres password=secret dbname=mydb"
}
```

The connection string is the standard `tokio_postgres` form. SSH tunneling and TLS features are compiled in via `my-postgres`.

## Running

### From source

```sh
cargo run --release
```

Then open <http://localhost:8000/>.

### Docker

`wwwroot/` must exist before the image is built — the Dockerfile copies it in. It is committed to the repo, so a plain checkout is enough; rebuild it only when the UI changes (see below).

```sh
cargo build --release
docker build -t postgres-mcp-server .
docker run --rm -p 8000:8000 -v ~/.postgres-mcp-server:/root/.postgres-mcp-server postgres-mcp-server
```

## Building the UI

The UI is a separate Dioxus (WASM) crate in [`ui/`](ui/) with its own `Cargo.lock`; it is not a workspace member. `build.sh` compiles it and copies the result into `wwwroot/`, which is committed alongside the source.

```sh
cd ui
./build.sh          # dx build --release --web  ->  ../wwwroot
```

Use `dx build` / `dx serve` for that crate — never `cargo build`. CI does not build the UI, so **commit `wwwroot/` whenever you change `ui/`**.

## Connecting an MCP client

Point your MCP client at `http://localhost:8000/mcp`. Example for Claude Desktop's `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "postgres": {
      "url": "http://localhost:8000/mcp"
    }
  }
}
```

The session is established automatically on first request (`initialize`); subsequent calls reuse the `mcp-session-id` header.

## How a request flows

1. Client → `POST /mcp` with `tools/call` for `sql_request`.
2. [`PostgresMcpService::execute_tool_call`](src/mcp_service/sql_request.rs) receives the SQL string.
3. [`sql_guard::classify`](src/sql_guard/classifier.rs) decides read vs write; a write with the window closed is refused here, logged, and never reaches Postgres.
4. [`PostgresAccess::do_request`](src/postgres/postgres.rs) executes it via `MyPostgres::execute_sql_as_vec` with a 10s timeout.
5. Each row is converted to a JSON value array; column names are captured once.
6. The outcome (rows, duration, error) is recorded in [`sql_log`](src/sql_log/) and the JSON is returned through the MCP middleware.

## HTTP API

| Route | Purpose |
|---|---|
| `GET /api/Settings` | write-access state + seconds left |
| `POST /api/Settings/McpWrites` | `{ "enabled": bool }` — open/close the 10-minute window |
| `GET /api/Requests` | last 100 SQL requests, newest first |
| `/swagger` | generated API docs |

## Limitations

- **No parameterized queries** — the SQL string is executed as-is.
- **Unauthenticated** — anyone who can reach the port can query, and can open the write window. Do not expose it to untrusted networks.
- **One statement per call** — several statements separated by `;` are refused; the extended protocol cannot run them.
- **10s query timeout** is hardcoded.
- **Single database** per server instance.

## Project layout

```
src/
├── main.rs              # bootstrap
├── settings.rs          # ~/.postgres-mcp-server reader
├── app/                 # AppContext: postgres handle, write window, request log
├── http_server/         # MyHttpServer + controllers/swagger/MCP/static wiring
├── mcp_service/         # sql_request tool + write_access_policy prompt
├── postgres/            # SQL execution + row → JSON conversion
├── sql_guard/           # read/write classifier + the write gate
└── sql_log/             # in-memory ring buffer of the last 100 requests
ui/                      # Dioxus WASM admin UI  ->  builds into wwwroot/
wwwroot/                 # built UI, committed; served at /
```

## Dependencies

- [`mcp-server-middleware`](https://github.com/my-ai-utils/mcp-server-middleware) — MCP protocol handling
- [`my-http-server`](https://github.com/MyJetTools/my-http-server) — HTTP runtime
- [`my-postgres`](https://github.com/MyJetTools/my-postgres) — Postgres client
- [`my-ai-agent`](https://github.com/my-ai-utils/my-ai-agent) — JSON schema derivation for tool I/O
