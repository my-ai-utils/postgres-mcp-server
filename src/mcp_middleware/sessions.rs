use std::collections::HashMap;

use rust_extensions::date_time::DateTimeAsMicroseconds;
use tokio::sync::Mutex;

use crate::mcp_middleware::McpSocketUpdateEvent;

pub struct McpSession {
    pub version: String,
    pub create: DateTimeAsMicroseconds,
    pub last_access: DateTimeAsMicroseconds,
    pub sender: Option<tokio::sync::mpsc::Sender<McpSocketUpdateEvent>>,
}

impl Drop for McpSession {
    fn drop(&mut self) {
        if let Some(sender) = self.sender.take() {
            tokio::spawn(async move {
                let _ = sender.send(McpSocketUpdateEvent::Shutdown).await;
            });
        }
    }
}

pub struct McpSessions {
    data: Mutex<HashMap<String, McpSession>>,
}

impl McpSessions {
    pub fn new() -> Self {
        Self {
            data: Mutex::new(HashMap::new()),
        }
    }

    pub async fn generate_session(&self, version: String, now: DateTimeAsMicroseconds) -> String {
        let id = uuid::Uuid::new_v4().to_string();

        let mut write_access = self.data.lock().await;

        write_access.insert(
            id.to_string(),
            McpSession {
                version,
                create: now,
                last_access: now,
                sender: None,
            },
        );

        id
    }

    pub async fn subscribe_to_notifications(
        &self,
        session_id: &str,
    ) -> Option<tokio::sync::mpsc::Receiver<McpSocketUpdateEvent>> {
        let mut write_access = self.data.lock().await;
        let session = write_access.get_mut(session_id)?;
        let (sender, receiver) = tokio::sync::mpsc::channel(32);
        session.sender = Some(sender);
        Some(receiver)
    }

    pub async fn check_session_and_update_last_used(
        &self,
        session_id: &str,
        now: DateTimeAsMicroseconds,
    ) -> bool {
        let mut write_access = self.data.lock().await;

        if let Some(session) = write_access.get_mut(session_id) {
            session.last_access = now;
            return true;
        }

        false
    }
}
