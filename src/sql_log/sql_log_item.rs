use rust_extensions::date_time::DateTimeAsMicroseconds;

/// How a request ended. `Blocked` never reaches Postgres — it is a refusal by
/// the write gate, and it is in the log precisely so a refusal is visible rather
/// than looking like the tool silently did nothing.
#[derive(Debug, Clone)]
pub enum SqlRequestStatus {
    Ok { rows: usize },
    Error { message: String },
    Blocked { message: String },
}

impl SqlRequestStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ok { .. } => "ok",
            Self::Error { .. } => "error",
            Self::Blocked { .. } => "blocked",
        }
    }
}

#[derive(Debug)]
pub struct SqlLogItem {
    pub id: u64,
    pub started: DateTimeAsMicroseconds,
    pub sql: String,
    pub is_write: bool,
    /// `None` for requests the gate refused — they never ran.
    pub took_micros: Option<u64>,
    pub status: SqlRequestStatus,
}
