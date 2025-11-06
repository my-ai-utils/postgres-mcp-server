use my_postgres::PostgresSettings;
use serde::*;

#[derive(Debug, Serialize, Deserialize, Clone, my_settings_reader::SettingsModel)]
pub struct SettingsModel {
    pub postgres_url: String,
}

#[async_trait::async_trait]
impl PostgresSettings for SettingsReader {
    async fn get_connection_string(&self) -> String {
        let read_access = self.settings.read().await;
        read_access.postgres_url.clone()
    }
}
