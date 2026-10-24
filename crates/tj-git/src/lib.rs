pub mod provider;
pub mod providers;
pub mod slug;
pub mod worktree;

pub use provider::{CreatePrOptions, GitProvider, GitProviderConfig, PrInfo, PrStatus};
pub use worktree::{WorktreeInfo, WorktreeManager};

use anyhow::Result;
use std::path::Path;

pub fn create_git_provider_from_repo(repo_path: &Path) -> Result<Option<Box<dyn GitProvider>>> {
    let config_path = repo_path.join(".turbojet").join("git-provider.json");
    if !config_path.exists() {
        return Ok(None);
    }

    let raw = std::fs::read_to_string(&config_path)?;
    let config: providers::forgejo::ForgejoConfig = serde_json::from_str(&raw)?;
    let provider = providers::forgejo::ForgejoProvider::from_config(config)?;
    Ok(Some(Box::new(provider)))
}

pub async fn create_git_provider(
    config: &GitProviderConfig,
    secrets_store: &dyn tj_secrets::SecretsStore,
    project_id: &str,
) -> Result<Box<dyn GitProvider>> {
    tracing::debug!(
        provider_type = %config.provider_type,
        base_url = %config.base_url,
        owner = %config.owner,
        repo = %config.repo,
        api_token_key = %config.api_token_key,
        project_id = %project_id,
        "Creating git provider from config with secrets resolution"
    );

    let token = secrets_store
        .get(project_id, &config.api_token_key)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Secret '{}' not found for project '{}'",
                config.api_token_key,
                project_id
            )
        })?;

    tracing::debug!(
        provider_type = %config.provider_type,
        project_id = %project_id,
        "Secret resolved, dispatching provider creation"
    );

    match config.provider_type.as_str() {
        "forgejo" => {
            let provider =
                crate::providers::forgejo::ForgejoProvider::from_config_and_token(config, token)?;
            Ok(Box::new(provider))
        }
        other => Err(anyhow::anyhow!(
            "Unsupported git provider type: '{}'. Supported: forgejo",
            other
        )),
    }
}
