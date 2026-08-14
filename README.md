# postgres-mcp-server

MCP (Model Context Protocol) server that exposes PostgreSQL databases as a single tool call. An LLM client (Claude Desktop, Claude Code, Cursor, any MCP-compatible agent) connects to this server, sends arbitrary SQL, and gets the result back as JSON.

It also serves a small web UI that shows what the agent has been running and gates write access behind an explicit, time-limited grant.

## What it does

- Listens on HTTP `:8000` and serves **one MCP endpoint per configured database**, each on its own path (`POST /mcp`, `POST /mcp-reporting`, …).
- Exposes two tools per endpoint, both bound to *that endpoint's* database and neither taking a database argument:
  - **`sql_request`** — accepts a `sql_request: string`, runs it, returns rows as JSON.
  - **`db_stats`** — returns collected statistics (table sizes, heaviest statements, connections, throughput) from a background cache, so it costs the database nothing. See *Statistics*.
- Exposes one MCP prompt per endpoint: **`write_access_policy`** — explains the write gate to the agent.
- Collects per-database statistics in the background and keeps **3 days of history** in a local [`redb`](https://www.redb.org) file.
- Serves the admin UI at `/` (requests + write access) and `/stats` (statistics), Swagger at `/swagger`.

## Several databases, one server

Each database is mounted on its own path and is a self-contained MCP server from the client's side. Hand `/crm` to one project and `/billing` to another and neither can see the other: the tool takes no database argument, it is bound to a single connection at startup, and **nothing on the MCP surface mentions the other mounts** — not the instructions, not the tool description, not a refusal message. No MCP method lists the databases, and an endpoint never enumerates its siblings.

The admin UI is the other side of that: it shows *all* of them — a single **Write access** card with one row per database (description, mount path, its own state pill, countdown and *Enable for 10 min* / *+10 min* / *Disable* buttons), and one request log covering all of them with a `Database` column.

The admin API is the deliberate exception, and it is not gated: `GET /api/Settings` lists every mount path and description to anyone who can reach the port, and `POST /api/Settings/McpWrites` will open any of their write windows. `GET /api/Stats` widens that considerably — it reports **every** database's table names and sizes, its heaviest statements' SQL text and the queries running on it right now. The path split separates *clients* from each other; it is not an authorization boundary. See *Limitations*.

Each database also gets its own:

- Postgres connection (its `application_name` carries the mount path, so a session on the server side says which endpoint opened it),
- write-access window — enabling writes on one leaves the others closed,
- MCP session pool, since the middlewares are independent.

## Write access

Read-only SQL runs at any time. Anything that writes is **refused by default**; the user grants writes from that database's row in the UI's *Write access* card. Every click adds **10 minutes** on top of whatever is left, so pressing it three times buys 30 minutes; *Disable* resets it to closed. The window auto-expires, and a restart always comes up disabled — it is runtime-only state and is never persisted. The window belongs to **one database**; there is no server-wide switch.

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

The SQL text is stored truncated: anything over **4096 bytes** is cut on a char boundary and gets a trailing `…`, in the API and the UI alike. The tool accepts arbitrary SQL, so 100 unbounded statements pinned in memory would be a real footgun.

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

`columns` is read off the **first returned row**, so a query that matched nothing comes back as `{"columns": [], "rows": []}` — with no row to read them from, the column names are not known. That is also the shape of every write without `RETURNING`.

Types are mapped per Postgres column type: `bool`, `int2`/`int4`/`int8`/`oid`, `float4`/`float8`, `text`/`varchar`/`bpchar`/`name`, `uuid`, `timestamp`/`timestamptz`, `date`/`time`, `json`/`jsonb`, and `bytea` as a `\x…` hex string. Everything else is unmapped and renders as the placeholder `[unsupported pg type: <name>]` — notably **`numeric`/`decimal`**, and also `interval`, `timetz`, the network types, enums and every array type. Cast those in the query (`price::float8`, `price::text`) if you need the value.

`NULL` becomes a JSON null in a mapped column (so does a value the mapping fails to read). In an unmapped column it does not: that branch never inspects the value, so a NULL `numeric` also comes back as the placeholder string.

## Statistics

The server polls every configured database in the background and publishes the result to the UI's *Statistics* page, to `GET /api/Stats`, and to the `db_stats` MCP tool. **Reading any of those three is a pure cache read** — the 5-second UI poller and an agent's tool call never turn into catalog queries, so the collection cost is fixed no matter who is watching.

### There is no CPU metric in Postgres

Not for a superuser, not for `pg_monitor`: no system view exposes host CPU at all. Reading it needs an agent on the machine or an extension like `pg_proctab`, and this server has neither. Everything under *load* is therefore a **proxy**, and the UI says so in each tooltip:

| Figure | What it really is |
|---|---|
| **`busyBackends`** | Δ`pg_stat_database.active_time` over wall-clock — backend-seconds of execution per second. `0.20` means the database was executing something 20% of the time; `3.0` means three backends were busy on average. Counts I/O and lock waits as busy, and sees nothing outside this database. Needs Postgres **14+**. |
| **`execMsPerSec`** (per statement) | The same idea from `pg_stat_statements`: milliseconds of execution per wall-clock second. `1000` is one backend saturated by that statement alone. This is what answers *which* query is burning the server. |
| `activeTime` / `sessionTime` | The raw counters the first row is derived from. |

Sizes, by contrast, are exact and need no privileges: `pg_total_relation_size` split into heap / indexes / TOAST, plus row estimates, scan counts and last vacuum/analyze, for the **top 25 tables by total size** (the response says `top 25 of 812`, never implying the list is complete). Row counts are planner estimates from `pg_stat_user_tables`, not `count(*)` — exact counts would mean a full scan of every listed table, once a minute.

### Who is connected

The connection *counts* answer "how many"; the **connections list** answers "which". One row per client backend of that database, from `pg_stat_activity`: user and application, where it connected from (`local` for the unix socket), its state, how long it has been in that state, how long the connection itself has been open, what it is waiting on, and its current statement — or, on a backend that is not active, the last one it ran.

The list is ordered active → `idle in transaction` → idle, longest-in-state first, and cut at **100 rows**; what the cap drops is the dull end of it, and the number shown is always published against `inThisDb`, the total it was cut from. It is also the only place an `idle in transaction` backend turns up: it is in no slow-query list, because it is not running anything — while it goes on holding every lock it took and pinning the xmin horizon against `VACUUM`. The UI marks that column once a backend has sat there for 5 seconds and reddens it at a minute.

This server's own collector connection is in the list, tagged `collector`, rather than hidden: it holds a `max_connections` slot like any other client, and dropping it would leave the list one shorter than the count beside it with nothing to explain the difference. (The *state* counts do exclude it, and say so — a connection that is `active` because it is running the query that measures activity would otherwise report one active backend forever on an idle database.)

### What an admin account changes

Only the parts that read *other users'* rows. Postgres returns a row per backend and per statement to everyone, but blanks `state`, `query` and `wait_event` for backends the account does not own unless it is a member of **`pg_monitor`** or **`pg_read_all_stats`** (superusers inherit both).

So an ordinary account still gets correct sizes and correct database-wide counters, but an undercounted and textless view of activity. That is reported rather than hidden: `canReadAllStats` travels to the UI as a `limited stats` badge, and the counts it undermines are published next to `stateUnknown` — the number of backends this account cannot see — with a banner naming the role to grant.

`pg_stat_statements` is a separate matter: it must be **installed** (`shared_preload_libraries`, then `CREATE EXTENSION`) and needs Postgres **13+** for the `total_exec_time` column. When it is missing the *load* section comes back `unavailable` with a reason saying which of the two is the problem, rather than as an empty list.

### Section states

Every section reports one of `pending` (no tick yet), `ready`, or `unavailable` **with a reason**. They are kept distinct on purpose: "the poller has not run", "the extension is not installed" and "this database has no tables" all look identical once flattened to *no data*, and each calls for a different reaction. A database that is simply unreachable reports the driver error once in `lastError` instead of repeating it in all four sections.

Numbers are `null` rather than `0` when Postgres genuinely cannot answer — `idx_scans` on a table no index was ever used on, `query` on a backend this account may not inspect, `busyBackends` before Postgres 14. The UI renders those as `—`. A zero would read as "nothing happened" when the truth is "this server, or this account, cannot tell you".

### Rates, not counters

Everything in `pg_stat_database` and `pg_stat_statements` is cumulative since the last reset, which answers "how many commits has this database ever done" — a question nobody is asking. The collector keeps the previous sample and publishes the difference, so `commitsPerSec`, `cacheHitRatio` and `busyBackends` describe the last window rather than all time. (The lifetime cache hit ratio is also reported, labelled as such: it is nearly always a flattering 0.99 because it is dominated by whatever ran on the day the statistics were last reset.)

If a counter goes backwards, or `stats_reset` changes between two samples, that window's rates are reported as **unknown** rather than as a negative rate or a fabricated zero. The next tick recovers on its own.

### Collection cadence

| Timer | Every | What |
|---|---|---|
| `DbStatsFast` | **5 s** | `pg_stat_activity` counts + connection list + longest queries, `pg_stat_database` counters → also appended to history |
| `DbStatsSlow` | **60 s** | capabilities, table sizes, `pg_stat_statements` |
| `DbStatsHourlyHistory` | **1 h** | appends table sizes + statement costs to history, then deletes everything past the retention horizon |

Databases are collected **concurrently**, so one unreachable mount cannot hold up another's numbers. Each query is bounded twice: the driver's own 5-second timeout, plus a hard 10-second ceiling — `my-postgres` retries underneath its timeout, and against an unreachable host a single call was measured at ~85 seconds, which is long enough to wedge the collection timer entirely.

Statistics queries never go through [`sql_guard`](src/sql_guard/) and are **never written to the request log**. They are not the agent's SQL, and a 5-second poller would evict the agent's own 100 entries within minutes.

### History

Kept for **3 days** in a single file, swept hourly:

- **load samples** every 5 seconds (`busyBackends`, transaction and I/O rates, cache hit ratio, connection counts, database size),
- **table sizes** and **statement costs** hourly — a table's size over three days is a growth curve, and sampling it 60 times an hour would store the same number 60 times while repeating every statement's SQL text alongside it.

A tick against an unreachable database writes **nothing**, so the series has a gap rather than a run of zeroes that would plot as "no load".

Read it at `GET /api/Stats/History` (see *HTTP API*). The UI's *Metrics history* card shows whether anything is actually being recorded — a full disk or a read-only home directory otherwise looks exactly like a healthy server, because every live card keeps updating.

#### Why redb and not SQLite

The store had to be **pure Rust, with no Rust→C marshalling**. That rules out every SQLite option: `rusqlite`/`libsqlite3-sys` compile the C amalgamation, `async-sqlite` wraps `rusqlite`, and so does the in-house `my-sqlite` — so the library this project would otherwise have used is excluded by the constraint, not by its API. `turso`, the pure-Rust SQLite rewrite, is still pre-1.0 and pulls `mimalloc` (C) in its default features; `sled` has no release since 2021.

Losing SQL costs nothing here, because none of the three access patterns needs it: append a sample, scan a time window, delete everything past the horizon. Those are range operations on an ordered B-tree, which is what `redb` is.

Rows are keyed `(unix_micros, db_path)`, in that order, so retention is **one bounded range sweep** covering every mount at once. Keyed the other way round the sweep would have to be repeated per configured mount, and rows belonging to a mount since removed from the settings file would never be collected at all — a leak that only shows up months later.

#### Where the file lives

`~/.postgres-mcp-server-metrics.redb`, next to the settings file. `POSTGRES_MCP_METRICS_PATH` overrides it (`~` is expanded either way).

It is an environment variable rather than a settings key because every field in the settings model is required with no defaults anywhere — adding one there would stop every existing settings file from parsing, for a path that has a perfectly good default. **In Docker it matters**: the default lands inside the container's `/root` and is lost on restart, so point the variable at a mounted volume if you want history to survive.

A history file that cannot be opened **disables history and is reported**, but never stops the boot: proxying SQL and gating writes do not depend on it.

## Configuration

Settings are read from `~/.postgres-mcp-server`, parsed as YAML (JSON is valid YAML, so a braced JSON file works just as well):

```yaml
databases:
- path: /mcp
  conn_string: "host=localhost user=postgres password=secret dbname=mydb"
  description: "Main production DB"
- path: /mcp-reporting
  conn_string: "host=localhost user=readonly password=secret dbname=reports"
  description: "Read-only reporting replica"
```

If the file cannot be read at all, the server falls back to fetching the same YAML over HTTP from `SETTINGS_URL`, and then keeps refreshing from that URL rather than from the file — a settings file created afterwards is not picked up until a restart. With neither present, startup prints `Can not load settings from file` on stderr and then panics on `Environment variable SETTINGS_URL is not set`.

One entry per database. All three fields are required, on every entry, and there are **no defaults anywhere in this model**: a missing key is a typo, not a request for a fallback, so the server refuses to start and says which entry is wrong. Also refused at boot:

- a `databases:` list that is present but empty — at least one entry is required,
- a path already used by another entry (compared case-insensitively, the way requests are routed),
- `/`, `/api…` or `/swagger…`, which belong to the UI, the admin API and swagger,
- an empty `conn_string` or `description`.

A path may be written without the leading slash or with a trailing one (`mcp-reporting/`); it is normalized to `/mcp-reporting`. Note that this normalization applies to the settings file, not to the URL a client uses — see *Connecting an MCP client*.

`description` is required rather than optional because it is the whole identity of the endpoint: it is the MCP server name the client sees (`Postgres MCP Server (<description>)`), it goes into the MCP `instructions` verbatim, it opens the `write_access_policy` prompt, it is named in every write refusal, and it labels the database's row in the UI. An endpoint with a blank one is an endpoint nobody can identify. Descriptions are *not* checked for uniqueness, but since the agent is told to ask the user for "the row for `<description>`", two identical ones make that instruction ambiguous — keep them distinct.

The connection string is the standard `tokio_postgres` form. SSH tunneling and TLS features are compiled in via `my-postgres` (`sslmode=require` turns TLS on; `ssh=user@host:port` opens a tunnel).

Its *content* is not validated here — only that it is non-empty. `my-postgres` parses it at startup and unwraps `host`, `dbname`, `user` and `password`, so a string missing any of those four takes the **whole process** down with a bare `Option::unwrap()` panic pointing into the driver, before the port is even bound. (`port` may be omitted.) An unreachable host is a different matter: that mount just retries in the background and the rest of the server runs normally.

### What a running server picks up, and what it does not

The connection string is resolved per mount on every **(re)connect** — not read from disk at that moment, but from the copy of the settings the process holds in memory, which `my-settings-reader` refreshes from the file on a 60-second timer. So an edited `conn_string` is picked up without a restart, but only on the first reconnect after the next refresh, and *nothing forces a reconnect* — a healthy connection keeps the old string until it drops. If the edited file is invalid YAML the refresh is skipped (one line on stderr) and the previous settings stay in force; the same file at boot aborts the start instead.

Everything else needs a restart:

- **`path`** — each one is an HTTP middleware registered at startup.
- **`description`** — read once at startup and baked into that endpoint's MCP server name and `instructions`, its prompt and its UI row.
- **removing a database** — deleting the entry does not take the endpoint down, and does not even cut the connection: a path that is no longer in the file, or whose `conn_string` has been blanked, falls back to the connection string it had at startup rather than to an empty one. **Editing the settings file is not a way to revoke access; only a restart is.**

> **Migrating from the single-database form.** `postgres_conn_string: "…"` is gone — wrap it in a `databases` entry as above, and give that entry `path: /mcp`: that was the hard-coded path of the single-database server, so keeping it means existing client URLs (`http://localhost:8000/mcp`) go on working. Until you convert the file the server does not start — it no longer deserializes, and the process aborts while parsing with ``Invalid yaml format of file: …. Err: missing field `databases` `` — *before* any of the per-entry checks above, so the message names no entry. A `postgres_conn_string:` key left behind *next to* a valid `databases:` list is simply ignored, with no warning. The admin API changed with it: `GET /api/Settings` now returns `{ "databases": [ … ] }` instead of a flat `{ mcpWritesEnabled, mcpWritesRemainingSecs }`, and `POST /api/Settings/McpWrites` now requires a `path` alongside `enabled`.

## Running

### From source

```sh
cargo run --release
```

Then open <http://localhost:8000/>.

### Docker

`wwwroot/` must exist before the image is built — the Dockerfile copies it in. It is committed to the repo, so a plain checkout is enough; rebuild it only when the UI changes (see below).

`Dockerfile` and `.github/workflows/release.yaml` are **generated**: `build.rs` runs `ci-utils`' `CiGenerator` on every `cargo build` and rewrites both, so hand-edits to either are silently reverted. The `COPY ./wwwroot ./wwwroot` line exists because `build.rs` declares `.add_docker_copy_file("./wwwroot", "./wwwroot")` — change it there, not in the Dockerfile.

```sh
cargo build --release
docker build -t postgres-mcp-server .
docker run --rm -p 8000:8000 \
  -v ~/.postgres-mcp-server:/root/.postgres-mcp-server \
  -v postgres-mcp-metrics:/data \
  -e POSTGRES_MCP_METRICS_PATH=/data/metrics.redb \
  postgres-mcp-server
```

The volume and `POSTGRES_MCP_METRICS_PATH` are only needed if the 3 days of metrics history should survive a restart — without them the file is written inside the container and thrown away with it. Everything else keeps working either way; see *Statistics → Where the file lives*.

## Building the UI

The UI is a separate Dioxus (WASM) crate in [`ui/`](ui/) with its own `Cargo.lock`; it is not a workspace member. `build.sh` compiles it and copies the result into `wwwroot/`, which is committed alongside the source.

```sh
./build-ui.sh            # from the repo root (thin wrapper), or:
cd ui && ./build.sh      # dx build --release --web -> cache-bust index.html -> ../wwwroot
```

It needs `dx` (**0.7.10** — it must match the `dioxus` version in `ui/Cargo.toml`, or dx refuses to build) and `python3`: `build.sh` runs `build.py` to append cache-busting query strings to the asset URLs in `index.html`. It also does `rm -rf ../wwwroot` before copying, so that directory is replaced wholesale — anything put there by hand is gone after a build.

`build.sh` wipes **dx's own output directory** first, too. dx content-hashes the js/wasm bundle names and never removes the previous ones, so after two builds its output holds two of each and the `cp -R` would copy all of them into `wwwroot` to be committed — hundreds of kilobytes of wasm that `index.html` does not reference. The wipe is what makes "one build, one bundle" true; if you invoke `dx build` by hand, expect to clean up after it.

The stylesheet is generated too: `ui/build.rs` concatenates `ui/css/01-tokens.css` … `06-stats.css` into `ui/public/assets/app.css`. That file is committed and so looks like a source file, but editing it is pointless — the next build overwrites it. A new `.css` file is picked up only once it is added to the `CssCompiler` chain in `ui/build.rs`.

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

**The URL must be a mount path exactly.** Case does not matter (`/MCP` reaches `/mcp`), a trailing slash does: `http://localhost:8000/mcp/` matches no MCP middleware and falls through to the SPA fallback, which answers `index.html` with **HTTP 200** for any method. A client pointed at a slightly wrong path therefore gets an HTML page where it expected JSON-RPC — a parse or protocol error, never a 404 — so check the path first. (The leading/trailing-slash normalization in *Configuration* applies to the settings file, not to this URL.)

The session is established automatically on first request (`initialize`); subsequent calls reuse the `mcp-session-id` header. Each endpoint keeps its own session registry, so an id minted on `/mcp` carries no state on `/mcp-reporting` — it is adopted there as a fresh, unrelated session rather than rejected.

## How a request flows

1. Client → `POST /mcp` (or any other configured path) with `tools/call` for `sql_request`. The path selects the [`DbContext`](src/app/db_ctx.rs); the tool has no database argument.
2. [`PostgresMcpService::execute_tool_call`](src/mcp_service/sql_request.rs) receives the SQL string.
3. [`sql_guard::classify`](src/sql_guard/classifier.rs) decides read vs write; a write with *that database's* window closed is refused here, logged, and never reaches Postgres.
4. [`PostgresAccess::do_request`](src/postgres/postgres.rs) executes it via `MyPostgres::execute_sql_as_vec` with a 10s timeout.
5. Each row is converted to a JSON value array; the column names are emitted once, taken from the first returned row — so a result with no rows carries no column names either.
6. The outcome (rows, duration, error) is recorded in [`sql_log`](src/sql_log/) and the JSON is returned through the MCP middleware.

## HTTP API

| Route | Purpose |
|---|---|
| `GET /api/Settings` | every configured database: path, description, write-access state + seconds left |
| `POST /api/Settings/McpWrites` | `{ "path": string, "enabled": bool }` — open/close the 10-minute window of one database. `200` returns the same body as `GET /api/Settings`, so the new countdown needs no follow-up read; `404` if that path is not configured |
| `GET /api/Requests` | last 100 SQL requests across all databases, newest first; each carries the `db` it ran on |
| `GET /api/Stats` | last collected statistics for every database, plus the state of the history file. A pure cache read — it never queries Postgres |
| `GET /api/Stats/History` | `?path=/mcp&hours=3&section=load\|tables\|statements` — one database's recorded series, oldest first. `hours` is clamped to 1…72; `404` if the path is not a configured mount, while a disabled or unreadable history file is a `200` carrying an `error` and empty series, because the mount itself is fine |
| `/swagger` | generated API docs |

## Limitations

- **No parameterized queries** — the SQL string is executed as-is.
- **Unauthenticated** — anyone who can reach the port can query any configured database, can open any write window, and can read `GET /api/Stats`, which reports every database's table names and sizes, its heaviest statements' SQL text and what is running on it right now. The path separation isolates *clients* from each other, not the server from the network. Do not expose it to untrusted networks.
- **One statement per call** — several statements separated by `;` are refused; the extended protocol cannot run them.
- **10s query timeout** is hardcoded.
- **Adding or removing a database needs a restart** — each one is an HTTP middleware registered at startup. Only `conn_string` is picked up live, and deleting an entry neither takes its endpoint down nor cuts its connection; see *What a running server picks up*.
- **The request log is shared**: 100 entries total, not 100 per database, so a busy database can push a quiet one's history out. It also truncates SQL at 4096 bytes.
- **`numeric`/`decimal` and other unmapped types** come back as `[unsupported pg type: …]`, not as values — cast them in the query. (This affects `sql_request` only; the statistics queries are hand-written with explicit casts and read their columns typed.)
- **No host CPU metric** — Postgres does not expose one to any role. `busyBackends` and `execMsPerSec` are execution-time proxies; see *Statistics*.
- **`busyBackends` needs Postgres 14+**, and the *load* section needs `pg_stat_statements` on **13+**. Both report `unavailable` with the reason rather than showing zeroes.
- **Statistics are per mount, scoped to that mount's database.** `pg_stat_activity` and `pg_stat_statements` are cluster-wide, but the connection list, the long-running queries and the statement list are filtered to the current database so an endpoint never leaks another database's SQL text. Connection *counts* are cluster-wide, since a number leaks nothing and `max_connections` is what they have to be read against.
- **History resolution is fixed** — 5 s for load, 1 h for sizes and statements — and retention is a hardcoded 3 days, swept hourly, so it can overshoot the horizon by up to an hour.

## Project layout

```
src/
├── main.rs              # bootstrap
├── settings.rs          # ~/.postgres-mcp-server reader + path/database validation
├── app/                 # AppContext: the databases + shared request log + metrics store
│                        #   db_ctx.rs: one database — connection, write window, stats cache
├── db_stats/            # statistics: one module per section (capabilities, activity,
│                        #   health, tables, statements), the per-database cache,
│                        #   the collection timers, the redb history store, and the
│                        #   wire model shared by /api/Stats and the db_stats tool
├── http_server/         # MyHttpServer + controllers/swagger/static wiring,
│                        #   plus one MCP middleware per database
├── mcp_service/         # sql_request + db_stats tools, write_access_policy prompt
│                        #   (one of each per database)
├── postgres/            # SQL execution + row → JSON conversion; query_typed is the
│                        #   separate, unlogged and ungated path the collector uses
├── sql_guard/           # read/write classifier + the write gate
└── sql_log/             # in-memory ring buffer of the last 100 requests
ui/                      # Dioxus WASM admin UI  ->  builds into wwwroot/
├── css/                 #   stylesheet sources (01-tokens … 06-stats);
│                        #   ui/build.rs concatenates them into
├── public/              #   public/assets/app.css — generated, committed
└── src/                 #   pages (/ and /stats) / components / api client
wwwroot/                 # built UI, committed; served at /
Dockerfile               # generated by build.rs (ci-utils) — do not hand-edit
```

## Dependencies

- [`mcp-server-middleware`](https://github.com/my-ai-utils/mcp-server-middleware) — MCP protocol handling
- [`my-http-server`](https://github.com/MyJetTools/my-http-server) — HTTP runtime
- [`my-postgres`](https://github.com/MyJetTools/my-postgres) — Postgres client
- [`my-ai-agent`](https://github.com/my-ai-utils/my-ai-agent) — JSON schema derivation for tool I/O
- [`redb`](https://www.redb.org) — pure-Rust embedded B-tree for the metrics history (see *Statistics → Why redb and not SQLite*)
