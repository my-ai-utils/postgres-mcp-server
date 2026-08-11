/// One section of a database's statistics — the tables card, the load card, and
/// so on.
///
/// The three states are kept apart on purpose. A section that has never been
/// collected, a section this account or this server version cannot produce, and a
/// section that is genuinely empty all look identical once they are flattened
/// into "no data", and the operator would have no way to tell "the poller has not
/// run yet" from "`pg_stat_statements` is not installed" from "this database has
/// no tables". Each of those calls for a different reaction, so each keeps its
/// own state and `Unavailable` always carries the reason verbatim.
#[derive(Debug, Clone)]
pub enum Section<T> {
    /// No tick has produced a value yet — the server has only just started, or
    /// the slow timer has not reached its first run.
    Pending,
    Ready(T),
    /// Cannot be collected: a missing extension, a server too old for the
    /// columns, a role the account is not a member of, or the query itself
    /// failed. The string is shown to the operator and handed to the agent as-is.
    Unavailable(String),
}

impl<T> Default for Section<T> {
    fn default() -> Self {
        Self::Pending
    }
}

impl<T> Section<T> {
    pub fn data(&self) -> Option<&T> {
        match self {
            Self::Ready(data) => Some(data),
            _ => None,
        }
    }

    pub fn reason(&self) -> Option<String> {
        match self {
            Self::Unavailable(reason) => Some(reason.clone()),
            _ => None,
        }
    }

    /// Wire value of the section's state. `"pending"` and `"unavailable"` are
    /// distinct on the wire for the same reason they are distinct here.
    pub fn state_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ready(_) => "ready",
            Self::Unavailable(_) => "unavailable",
        }
    }

    /// Rewraps a `Result` from one collection query.
    pub fn from_result(result: Result<T, String>) -> Self {
        match result {
            Ok(data) => Self::Ready(data),
            Err(err) => Self::Unavailable(err),
        }
    }
}
