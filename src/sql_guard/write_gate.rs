use crate::app::AppContext;

/// Gate for write SQL coming in over MCP. Writes must be explicitly enabled by
/// the admin on the UI; the window lasts 10 minutes (or until it is switched
/// off). Returns a tool-friendly error string when writes are disabled.
///
/// The wording matters: the string is handed straight back to the LLM as the
/// tool result, so it has to say what the user must do *and* tell the model not
/// to sit in a retry loop while it waits.
pub fn ensure_mcp_writes_enabled(app: &AppContext, reason: &str) -> Result<(), String> {
    if app.is_mcp_write_enabled() {
        return Ok(());
    }

    Err(format!(
        "This request was REFUSED because it writes to the database ({reason}), and write access \
         is currently DISABLED. Read-only queries (SELECT and friends) always work. To allow \
         writes, ask the user to open the Postgres MCP Server UI and click \"Enable for 10 min\" \
         on the Write access card; writes then stay on for 10 minutes. Do not retry until the \
         user confirms they have enabled it."
    ))
}
