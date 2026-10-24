use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitProviderConfig {
    pub provider_type: String,
    pub base_url: String,
    pub owner: String,
    pub repo: String,
    pub api_token_key: String,
    #[serde(default)]
    pub transport_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatePrOptions {
    pub repo_path: PathBuf,
    pub source_branch: String,
    pub target_branch: String,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub reviewers: Vec<String>,
    #[serde(default)]
    pub draft: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PrStatus {
    Open,
    Merged,
    Closed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrInfo {
    pub id: String,
    pub url: String,
    pub number: Option<u64>,
    pub status: PrStatus,
    #[serde(default)]
    pub draft: bool,
}

#[async_trait]
pub trait GitProvider: Send + Sync {
    async fn create_pr(&self, opts: CreatePrOptions) -> Result<PrInfo>;
    /// Idempotency lookup used before creating a PR. A lost HTTP response must
    /// reconcile against the remote instead of opening a duplicate PR.
    async fn find_pr(&self, source_branch: &str) -> Result<Option<PrInfo>>;
    async fn get_pr_status(&self, pr_id: &str) -> Result<PrStatus>;
    async fn update_pr_body(&self, _pr_number: u64, _body: &str) -> Result<()> {
        Ok(())
    }
    async fn mark_ready(&self, _pr_number: u64) -> Result<()> {
        Ok(())
    }
    fn provider_id(&self) -> &str;
}
