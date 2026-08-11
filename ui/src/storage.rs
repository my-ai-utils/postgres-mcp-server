const THEME_KEY: &str = "pg_mcp_theme";

/// Which Postgres server the statistics page was last looking at.
///
/// The server's *key* rather than its position, so a database added to the settings
/// file while the page is open cannot silently move the selection to a different
/// server. A key that no longer matches anything falls back to the first configured
/// server rather than showing nothing.
const SELECTED_SERVER_KEY: &str = "pg_mcp_selected_server";

fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

pub fn save_theme(theme: &str) {
    if let Some(s) = local_storage() {
        let _ = s.set_item(THEME_KEY, theme);
    }
}

pub fn load_theme() -> Option<String> {
    let s = local_storage()?;
    s.get_item(THEME_KEY).ok()?.filter(|v| !v.is_empty())
}

pub fn save_selected_server(key: &str) {
    if let Some(s) = local_storage() {
        let _ = s.set_item(SELECTED_SERVER_KEY, key);
    }
}

pub fn load_selected_server() -> Option<String> {
    let s = local_storage()?;
    s.get_item(SELECTED_SERVER_KEY)
        .ok()?
        .filter(|v| !v.is_empty())
}

/// The browser's own confirmation dialog.
///
/// Used rather than a hand-built modal because this is the one place in the UI where
/// a click changes something outside this server — `ALTER SYSTEM` reaches the whole
/// Postgres cluster. `window.confirm` blocks the page until it is answered, which is
/// exactly the property wanted: no chance of the request going out while the operator
/// is still reading what it does.
///
/// Returns `false` when there is no window to ask, so an environment without one
/// cannot be a way to skip the question.
pub fn confirm(message: &str) -> bool {
    web_sys::window()
        .and_then(|window| window.confirm_with_message(message).ok())
        .unwrap_or(false)
}

pub fn apply_theme(theme: &str) {
    if let Some(window) = web_sys::window() {
        if let Some(doc) = window.document() {
            if let Some(html) = doc.document_element() {
                let _ = html.set_attribute("data-theme", theme);
            }
        }
    }
}
