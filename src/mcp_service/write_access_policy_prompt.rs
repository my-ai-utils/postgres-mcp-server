use std::collections::HashMap;

use mcp_server_middleware::*;

pub struct WriteAccessPolicyPromptHandler;

impl PromptDefinition for WriteAccessPolicyPromptHandler {
    const PROMPT_NAME: &'static str = "write_access_policy";

    const DESCRIPTION: &'static str =
        "How write SQL is gated on this server: the user must grant write access from the UI (10-minute window). Read before sending any INSERT/UPDATE/DELETE/DDL through sql_request.";

    fn get_argument_descriptions() -> Vec<PromptArgumentDescription> {
        Vec::new()
    }
}

#[async_trait::async_trait]
impl McpPromptService for WriteAccessPolicyPromptHandler {
    async fn execute_prompt(
        &self,
        _model: &HashMap<String, String>,
    ) -> Result<PromptExecutionResult, String> {
        let body = r#"# Write Access Policy

`sql_request` runs read-only SQL at any time. Statements that write are
DISABLED by default. There is no password — the user must explicitly turn
write access on from the Postgres MCP Server UI:

> **UI → "Write access" card → click "Enable for 10 min".**

Each click adds **10 minutes** on top of whatever is left, so the user can
press it twice for 20 minutes. The window then auto-closes. **Disable**
resets it to closed immediately. A server restart always leaves write access
disabled.

## What counts as a write

The server allow-lists reads rather than deny-listing writes: only `SELECT`,
`WITH ... SELECT`, `EXPLAIN` (without `ANALYZE`), `SHOW`, `TABLE` and
`VALUES` run without the window. Everything else is treated as a write —
including cases that look like reads:

- `WITH x AS (INSERT ... RETURNING *) SELECT * FROM x` — a data-modifying CTE
- `SELECT * INTO new_table FROM t` — this creates a table
- `EXPLAIN ANALYZE <stmt>` — `ANALYZE` really executes the statement
- `SELECT ... FOR UPDATE` — takes row locks
- `SELECT nextval('s')` and other side-effecting functions
- `SET`, `BEGIN`, `DECLARE`, `CALL`, `DO`, `COPY`, and all DDL

Anything the server cannot parse is treated as a write, on purpose.

## Mentioning a write word is enough

On top of the above, the server refuses any statement whose **raw text** contains
`insert`, `update`, `delete`, `merge`, `truncate`, `drop`, `alter`, `create`,
`grant` or `revoke` as a whole word — **even inside a string literal or a
comment**. So this is refused despite only reading:

```sql
SELECT * FROM audit WHERE action = 'insert'
```

This over-blocks on purpose. If you hit it on a genuine read, you can often just
rephrase — the word only counts as a whole word, so `delete_flag`,
`updated_users` and `user_insert_log` are all fine. Comparing against a value
like `'insert'` can be rewritten, e.g. `action = concat('ins','ert')`, or the
user can simply grant write access for 10 minutes.

## Rules

1. **Never assume write access is on.** Reads always work; writes may not.
2. **If a request is refused** with "write access is currently DISABLED", do
   NOT retry in a loop. Tell the user to click "Enable for 10 min" in the UI,
   then continue once they confirm.
3. The 10-minute window can lapse mid-task. If a later write fails after
   earlier ones succeeded, the window probably expired — ask the user to
   re-enable rather than assuming the statement was wrong.
4. Send exactly **one statement** per call. Several statements separated by
   `;` are refused — the driver cannot run them anyway.
5. `sql_request` returns **rows returned**, not rows affected. Add
   `RETURNING` to a write if you need to see what it did."#;

        Ok(PromptExecutionResult {
            description: "How write SQL is gated (UI Write access card, 10-minute window)."
                .to_string(),
            message: body.to_string(),
        })
    }
}
