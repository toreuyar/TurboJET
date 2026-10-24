use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait SecretsStore: Send + Sync {
    async fn get(&self, project_id: &str, key: &str) -> Result<Option<String>>;
    async fn set(&self, project_id: &str, key: &str, value: &str) -> Result<()>;
    async fn delete(&self, project_id: &str, key: &str) -> Result<()>;
    async fn list(&self, project_id: &str) -> Result<Vec<String>>;
}
