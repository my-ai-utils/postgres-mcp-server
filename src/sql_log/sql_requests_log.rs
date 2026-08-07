use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use rust_extensions::date_time::DateTimeAsMicroseconds;

use super::{SqlLogItem, SqlRequestStatus};

const MAX_ITEMS: usize = 100;

/// The tool accepts arbitrary SQL, so a pathological statement could be
/// megabytes. 100 of those pinned in memory is a real footgun; the UI only ever
/// shows an ellipsized cell anyway.
const MAX_SQL_LEN: usize = 4096;

/// In-memory ring buffer of the last [`MAX_ITEMS`] SQL requests, across **all**
/// configured databases — one timeline, each entry tagged with the database it
/// ran against.
///
/// Written on every request, so this is not a read-mostly structure and ArcSwap
/// would be the wrong tool — a plain `parking_lot::Mutex` around the container
/// is what the performance guide calls for. Items are handed out as `Arc`s so a
/// snapshot never deep-clones the SQL text, and the lock is only ever held for
/// the container operation itself — never across serialization or an `.await`.
pub struct SqlRequestsLog {
    items: Mutex<VecDeque<Arc<SqlLogItem>>>,
    next_id: AtomicU64,
}

impl SqlRequestsLog {
    pub fn new() -> Self {
        Self {
            items: Mutex::new(VecDeque::with_capacity(MAX_ITEMS)),
            next_id: AtomicU64::new(1),
        }
    }

    pub fn add(
        &self,
        db_path: Arc<String>,
        sql: String,
        is_write: bool,
        started: DateTimeAsMicroseconds,
        took_micros: Option<u64>,
        status: SqlRequestStatus,
    ) {
        let item = Arc::new(SqlLogItem {
            id: self.next_id.fetch_add(1, Ordering::Relaxed),
            db_path,
            started,
            sql: truncate(sql),
            is_write,
            took_micros,
            status,
        });

        let mut items = self.items.lock();
        while items.len() >= MAX_ITEMS {
            items.pop_front();
        }
        items.push_back(item);
    }

    /// Snapshot, newest first.
    pub fn get_all(&self) -> Vec<Arc<SqlLogItem>> {
        let items = self.items.lock();
        items.iter().rev().cloned().collect()
    }
}

fn truncate(mut sql: String) -> String {
    if sql.len() <= MAX_SQL_LEN {
        return sql;
    }

    // Cut on a char boundary — SQL can hold any UTF-8.
    let cut = sql
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|i| *i <= MAX_SQL_LEN)
        .last()
        .unwrap_or(0);

    sql.truncate(cut);
    sql.push('…');
    sql
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_path() -> Arc<String> {
        Arc::new("/mcp".to_string())
    }

    fn add_n(log: &SqlRequestsLog, n: usize) {
        for i in 0..n {
            log.add(
                db_path(),
                format!("SELECT {}", i),
                false,
                DateTimeAsMicroseconds::now(),
                Some(1),
                SqlRequestStatus::Ok { rows: 0 },
            );
        }
    }

    #[test]
    fn keeps_only_the_last_100_newest_first() {
        let log = SqlRequestsLog::new();
        add_n(&log, 150);

        let items = log.get_all();
        assert_eq!(items.len(), MAX_ITEMS);
        assert_eq!(items[0].sql, "SELECT 149");
        assert_eq!(items[MAX_ITEMS - 1].sql, "SELECT 50");
    }

    #[test]
    fn ids_increment() {
        let log = SqlRequestsLog::new();
        add_n(&log, 3);

        let items = log.get_all();
        assert_eq!(items[0].id, 3);
        assert_eq!(items[2].id, 1);
    }

    #[test]
    fn long_sql_is_truncated_on_a_char_boundary() {
        let log = SqlRequestsLog::new();
        // Multi-byte chars, so a naive byte truncate would panic.
        log.add(
            db_path(),
            "😀".repeat(4000),
            false,
            DateTimeAsMicroseconds::now(),
            Some(1),
            SqlRequestStatus::Ok { rows: 0 },
        );

        let items = log.get_all();
        assert!(items[0].sql.len() <= MAX_SQL_LEN + 4);
        assert!(items[0].sql.ends_with('…'));
    }

    #[test]
    fn every_entry_remembers_its_database() {
        let log = SqlRequestsLog::new();

        for path in ["/mcp", "/mcp-reporting"] {
            log.add(
                Arc::new(path.to_string()),
                "SELECT 1".to_string(),
                false,
                DateTimeAsMicroseconds::now(),
                Some(1),
                SqlRequestStatus::Ok { rows: 0 },
            );
        }

        let items = log.get_all();
        assert_eq!(items[0].db_path.as_str(), "/mcp-reporting");
        assert_eq!(items[1].db_path.as_str(), "/mcp");
    }
}
