use my_http_server::HttpOutputProducer;

pub enum McpSocketUpdateEvent {
    Shutdown,
}

impl McpSocketUpdateEvent {
    fn is_shutdown(&self) -> bool {
        match self {
            Self::Shutdown => true,
        }
    }
}

pub async fn stream_updates(
    _producer: HttpOutputProducer,
    mut receiver: tokio::sync::mpsc::Receiver<McpSocketUpdateEvent>,
) {
    while let Some(value) = receiver.recv().await {
        if value.is_shutdown() {
            return;
        }
    }
}
