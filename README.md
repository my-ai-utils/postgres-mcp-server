# postgres-mcp-server

MCP (Model Context Protocol) server that exposes PostgreSQL databases as a single tool call. An LLM client (Claude Desktop, Claude Code, Cursor, any MCP-compatible agent) connects to this server, sends arbitrary SQL, and gets the result back as JSON.

It also serves a small web UI that shows what the agent has been running and gates write access behind an explicit, time-limited grant.

## What it does

- Listens on HTTP `:8000` and serves **one MCP endpoint per configured database**, each on its own path (`POST /mcp`, `POST /mcp-reporting`, …).
- Exposes one tool per endpoint: **`sql_request`** — accepts a `sql_request: string`, runs it against *that endpoint's* database, returns rows as JSON.
- Exposes one MCP prompt per endpoint: **`write_access_policy`** — explains the write gate to the agent.
- Serves the admin UI at `/`, Swagger at `/swagger`.

## Several databases, one server

Each database is mounted on its own path and is a self-contained MCP server from the client's side. Hand `/crm` to one project and `/billing` to another and neither can see the other: the tool takes no database argument, it is bound to a single connection at startup, and **nothing on the MCP surface mentions the other mounts** — not the instructions, not the tool description, not a refusal message. There is no endpoint that lists the databases.

The admin UI is the other side of that: it shows *all* of them — every database with its own write-access card, and one request log covering all of them with a `Database` column.

Each database also gets its own:

- Postgres connection (its `application_name` carries the mount path, so a session on the server side says which endpoint opened it),
- write-access window — enabling writes on one leaves the others closed,
- MCP session pool, since the middlewares are independent.

## Write access

Read-only SQL runs at any time. Anything that writes is **refused by default**; the user grants writes from that database's *Write access* card in the UI. Every click adds **10 minutes** on top of whatever is left, so pressing it three times buys 30 minutes; *Disable* resets it to closed. The window auto-expires, and a restart always comes up disabled — it is runtime-only state and is never persisted. The window belongs to **one database**; there is no server-wide switch.

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

The last **100** requests are kept in memory (`GET /api/Requests`, newest first) and shown in the UI: time, database, SQL, rows returned, duration, status. It is one timeline across every configured database — the operator's view, not a client's — and each entry carries the mount path it ran against. Refusals are logged too — a blocked request that left no trace would look like the tool silently did nothing. The log is in-memory only and empty after a restart.

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

Settings are read from `~/.postgres-mcp-server` (YAML — JSON is valid YAML, so the old brace form still parses):

```yaml
databases:
- path: /mcp
  conn_string: "host=localhost user=postgres password=secret dbname=mydb"
  description: "Main production DB"
- path: /mcp-reporting
  conn_string: "host=localhost user=readonly password=secret dbname=reports"
  description: "Read-only reporting replica"
```

One entry per database. All three fields are required, on every entry, and there are **no defaults anywhere in this model**: a missing key is a typo, not a request for a fallback, so the server refuses to start and says which entry is wrong. The same goes for these, all checked at boot:

- a path that is already used by another entry (compared case-insensitively, the way requests are routed),
- `/`, `/api…` or `/swagger…`, which belong to the UI, the admin API and swagger,
- an empty `conn_string` or `description`.

A path may be written without the leading slash or with a trailing one (`mcp-reporting/`); it is normalized to `/mcp-reporting`.

`description` is required rather than optional because it is what the agent is told the endpoint is bound to (it goes into the MCP `instructions` verbatim) and what labels the card in the UI. An endpoint with a blank one is an endpoint nobody can identify.

The connection string is the standard `tokio_postgres` form. SSH tunneling and TLS features are compiled in via `my-postgres`. It is re-read from the file per mount on every reconnect, so editing a `conn_string` is picked up without a restart — but adding or removing a `path` needs one, since each path is an HTTP middleware registered at startup.

> **Note.** The single-database `postgres_conn_string: "…"` form is gone. Wrap it in a `databases` entry as above.

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

Point your MCP client at the path of the database it should get — one entry per database, and a client only ever sees the one it was given:

```json
{
  "mcpServers": {
    "postgres": {
      "url": "http://localhost:8000/mcp"
    },
    "postgres-reporting": {
      "url": "http://localhost:8000/mcp-reporting"
    }
  }
}
```

The session is established automatically on first request (`initialize`); subsequent calls reuse the `mcp-session-id` header. Sessions belong to one endpoint — an id minted on `/mcp` means nothing on `/mcp-reporting`.

## How a request flows

1. Client → `POST /mcp` (or any other configured path) with `tools/call` for `sql_request`. The path selects the [`DbContext`](src/app/db_ctx.rs); the tool has no database argument.
2. [`PostgresMcpService::execute_tool_call`](src/mcp_service/sql_request.rs) receives the SQL string.
3. [`sql_guard::classify`](src/sql_guard/classifier.rs) decides read vs write; a write with *that database's* window closed is refused here, logged, and never reaches Postgres.
4. [`PostgresAccess::do_request`](src/postgres/postgres.rs) executes it via `MyPostgres::execute_sql_as_vec` with a 10s timeout.
5. Each row is converted to a JSON value array; column names are captured once.
6. The outcome (rows, duration, error) is recorded in [`sql_log`](src/sql_log/) and the JSON is returned through the MCP middleware.

## HTTP API

| Route | Purpose |
|---|---|
| `GET /api/Settings` | every configured database: path, description, write-access state + seconds left |
| `POST /api/Settings/McpWrites` | `{ "path": string, "enabled": bool }` — open/close the 10-minute window of one database; `404` if that path is not configured |
| `GET /api/Requests` | last 100 SQL requests across all databases, newest first; each carries the `db` it ran on |
| `/swagger` | generated API docs |

## Limitations

- **No parameterized queries** — the SQL string is executed as-is.
- **Unauthenticated** — anyone who can reach the port can query any configured database, and can open any write window. The path separation isolates *clients* from each other, not the server from the network. Do not expose it to untrusted networks.
- **One statement per call** — several statements separated by `;` are refused; the extended protocol cannot run them.
- **10s query timeout** is hardcoded.
- **Adding or removing a database needs a restart** — each one is an HTTP middleware registered at startup. Changing an existing `conn_string` does not.
- **The request log is shared**: 100 entries total, not 100 per database, so a busy database can push a quiet one's history out.

## Project layout

```
src/
├── main.rs              # bootstrap
├── settings.rs          # ~/.postgres-mcp-server reader + path/database validation
├── app/                 # AppContext: the databases + shared request log
│                        #   db_ctx.rs: one database — connection + its write window
├── http_server/         # MyHttpServer + controllers/swagger/static wiring,
│                        #   plus one MCP middleware per database
├── mcp_service/         # sql_request tool + write_access_policy prompt (per database)
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
