use serde::Deserialize;

/// Mirrors the server's `SqlRequestModel` (an item of `GET /api/Requests`).
///
/// Every optional field carries `#[serde(default)]` so a server that predates a
/// field still deserializes cleanly instead of failing the whole list.
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct SqlRequestModel {
    #[serde(default)]
    pub id: u64,
    /// rfc3339, e.g. `2026-07-16T12:04:31.123456+00:00`.
    #[serde(default)]
    pub started: String,
    #[serde(default)]
    pub sql: String,
    /// `"read"` | `"write"`. Part of the wire contract; the table does not
    /// surface it as its own column today.
    #[serde(default)]
    #[allow(dead_code)]
    pub kind: String,
    /// `"ok"` | `"error"` | `"blocked"`.
    #[serde(default)]
    pub status: String,
    /// Omitted by the server unless `status == "ok"`.
    #[serde(default)]
    pub rows: Option<u64>,
    /// Omitted by the server when `status == "blocked"` (nothing ran).
    #[serde(rename = "tookMicros", default)]
    pub took_micros: Option<u64>,
    /// Omitted by the server unless `status` is `"error"` or `"blocked"`.
    #[serde(default)]
    pub error: Option<String>,
}

impl SqlRequestModel {
    pub fn is_ok(&self) -> bool {
        self.status == "ok"
    }

    /// Just the clock part of the rfc3339 stamp: `12:04:31`.
    pub fn time_label(&self) -> &str {
        let Some(t_pos) = self.started.find('T') else {
            return self.started.as_str();
        };

        let rest = &self.started[t_pos + 1..];
        rest.get(..8).unwrap_or(rest)
    }

    /// The row count, but only when the statement actually ran and returned.
    pub fn rows_label(&self) -> String {
        match self.rows {
            Some(rows) if self.is_ok() => rows.to_string(),
            _ => "—".to_string(),
        }
    }

    /// `340µs` / `1.2ms` / `2.10s` — scaled to keep the column narrow.
    pub fn took_label(&self) -> String {
        let Some(micros) = self.took_micros else {
            return "—".to_string();
        };

        if micros < 1_000 {
            format!("{}µs", micros)
        } else if micros < 1_000_000 {
            format!("{:.1}ms", micros as f64 / 1_000.0)
        } else {
            format!("{:.2}s", micros as f64 / 1_000_000.0)
        }
    }

    /// Hover text for the status pill — the reason a request failed or was
    /// refused. `None` for a successful request.
    pub fn status_title(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

/// Envelope of `GET /api/Requests` — `{"items": [...]}`, newest first.
#[derive(Deserialize, Default)]
pub struct SqlRequestsResponse {
    #[serde(default)]
    pub items: Vec<SqlRequestModel>,
}
