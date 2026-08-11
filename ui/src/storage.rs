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

pub fn apply_theme(theme: &str) {
    if let Some(window) = web_sys::window() {
        if let Some(doc) = window.document() {
            if let Some(html) = doc.document_element() {
                let _ = html.set_attribute("data-theme", theme);
            }
        }
    }
}
