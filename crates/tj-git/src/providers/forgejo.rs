use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::provider::{CreatePrOptions, GitProvider, GitProviderConfig, PrInfo, PrStatus};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForgejoConfig {
    #[serde(rename = "type")]
    pub provider_type: String,
    pub forgejo_url: String,
    pub token_env: String,
    pub owner: String,
    pub repo: String,
}

pub struct ForgejoProvider {
    forgejo_url: String,
    owner: String,
    repo: String,
    token: String,
    client: Client,
}

impl ForgejoProvider {
    pub fn from_config(config: ForgejoConfig) -> Result<Self> {
        let token = std::env::var(&config.token_env)
            .with_context(|| format!("Environment variable {} not set", config.token_env))?;
        tracing::debug!(
            forgejo_url = %config.forgejo_url,
            owner = %config.owner,
            repo = %config.repo,
            "Creating ForgejoProvider from ForgejoConfig"
        );
        Ok(Self {
            forgejo_url: config.forgejo_url,
            owner: config.owner,
            repo: config.repo,
            token,
            client: Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(30))
                .build()?,
        })
    }

    pub fn from_config_and_token(config: &GitProviderConfig, token: String) -> Result<Self> {
        tracing::debug!(
            forgejo_url = %config.base_url,
            owner = %config.owner,
            repo = %config.repo,
            "Creating ForgejoProvider from GitProviderConfig with resolved token"
        );
        Ok(Self {
            forgejo_url: config.base_url.clone(),
            owner: config.owner.clone(),
            repo: config.repo.clone(),
            token,
            client: Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(30))
                .build()?,
        })
    }

    fn api_base(&self) -> String {
        let base = self.forgejo_url.trim_end_matches('/');
        format!("{}/api/v1/repos/{}/{}", base, self.owner, self.repo)
    }
}

#[derive(Deserialize)]
struct ForgejoPrResponse {
    number: u64,
    html_url: String,
    state: String,
    merged: Option<bool>,
    #[serde(default)]
    draft: Option<bool>,
    #[serde(default)]
    head: Option<ForgejoPrHead>,
}

#[derive(Deserialize)]
struct ForgejoPrHead {
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    r#ref: Option<String>,
}

#[async_trait]
impl GitProvider for ForgejoProvider {
    async fn create_pr(&self, opts: CreatePrOptions) -> Result<PrInfo> {
        let url = format!("{}/pulls", self.api_base());
        let body = serde_json::json!({
            "head": opts.source_branch,
            "base": opts.target_branch,
            "title": opts.title,
            "body": opts.body,
            "draft": opts.draft,
        });

        tracing::debug!(
            url = %url,
            source = %opts.source_branch,
            target = %opts.target_branch,
            title = %opts.title,
            draft = opts.draft,
            "Creating Forgejo PR"
        );

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("token {}", self.token))
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await
            .context("Failed to send Forgejo PR creation request")?;

        let status = resp.status();
        tracing::debug!(status = %status, "Forgejo PR creation response status");

        let resp: ForgejoPrResponse = resp
            .error_for_status()
            .context(format!("Forgejo PR creation failed with status {}", status))?
            .json()
            .await
            .context("Failed to parse Forgejo PR response")?;

        tracing::debug!(
            pr_number = resp.number,
            url = %resp.html_url,
            "Forgejo PR created"
        );

        Ok(PrInfo {
            id: resp.number.to_string(),
            url: resp.html_url,
            number: Some(resp.number),
            status: PrStatus::Open,
            draft: resp.draft.unwrap_or(opts.draft),
        })
    }

    async fn find_pr(&self, source_branch: &str) -> Result<Option<PrInfo>> {
        let url = format!("{}/pulls?state=all&limit=100", self.api_base());
        tracing::debug!(url = %url, source = %source_branch, "Looking up existing Forgejo PR");
        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("token {}", self.token))
            .header("Accept", "application/json")
            .send()
            .await
            .context("Failed to list Forgejo pull requests")?;
        let status = resp.status();
        let resp = resp
            .error_for_status()
            .context(format!("Forgejo PR lookup failed with status {}", status))?;
        let pulls: Vec<ForgejoPrResponse> = resp
            .json()
            .await
            .context("Failed to parse Forgejo pull request list")?;
        let found = pulls.into_iter().find(|pull| {
            pull.head.as_ref().is_some_and(|head| {
                head.r#ref.as_deref() == Some(source_branch)
                    || head
                        .label
                        .as_deref()
                        .is_some_and(|label| label.ends_with(&format!(":{source_branch}")))
            })
        });
        Ok(found.map(|pull| PrInfo {
            id: pull.number.to_string(),
            url: pull.html_url,
            number: Some(pull.number),
            status: if pull.merged.unwrap_or(false) {
                PrStatus::Merged
            } else if pull.state == "closed" {
                PrStatus::Closed
            } else {
                PrStatus::Open
            },
            draft: pull.draft.unwrap_or(false),
        }))
    }

    async fn get_pr_status(&self, pr_id: &str) -> Result<PrStatus> {
        let url = format!("{}/pulls/{}", self.api_base(), pr_id);

        tracing::debug!(url = %url, pr_id = %pr_id, "Getting Forgejo PR status");

        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("token {}", self.token))
            .header("Accept", "application/json")
            .send()
            .await
            .context("Failed to get Forgejo PR status")?;

        let status = resp.status();
        tracing::debug!(status = %status, "Forgejo PR status response");

        let resp: ForgejoPrResponse = resp
            .error_for_status()
            .context(format!(
                "Forgejo get PR status failed with status {}",
                status
            ))?
            .json()
            .await
            .context("Failed to parse Forgejo PR status response")?;

        let result = if resp.merged.unwrap_or(false) {
            PrStatus::Merged
        } else if resp.state == "closed" {
            PrStatus::Closed
        } else {
            PrStatus::Open
        };

        tracing::debug!(
            pr_id = %pr_id,
            state = %resp.state,
            merged = ?resp.merged,
            result = ?result,
            "Forgejo PR status resolved"
        );

        Ok(result)
    }

    async fn update_pr_body(&self, pr_number: u64, body: &str) -> Result<()> {
        let url = format!("{}/pulls/{}", self.api_base(), pr_number);
        let payload = serde_json::json!({
            "body": body,
        });

        tracing::debug!(
            url = %url,
            pr_number = pr_number,
            body_len = body.len(),
            "Updating Forgejo PR body"
        );

        let resp = self
            .client
            .patch(&url)
            .header("Authorization", format!("token {}", self.token))
            .header("Accept", "application/json")
            .json(&payload)
            .send()
            .await
            .context("Failed to send Forgejo PR body update")?;

        let status = resp.status();
        tracing::debug!(status = %status, "Forgejo PR body update response");

        resp.error_for_status().context(format!(
            "Forgejo PR body update failed with status {}",
            status
        ))?;

        tracing::debug!(pr_number = pr_number, "Forgejo PR body updated");
        Ok(())
    }

    async fn mark_ready(&self, pr_number: u64) -> Result<()> {
        let url = format!("{}/pulls/{}", self.api_base(), pr_number);
        let payload = serde_json::json!({
            "draft": false,
        });

        tracing::debug!(
            url = %url,
            pr_number = pr_number,
            "Marking Forgejo PR as ready"
        );

        let resp = self
            .client
            .patch(&url)
            .header("Authorization", format!("token {}", self.token))
            .header("Accept", "application/json")
            .json(&payload)
            .send()
            .await
            .context("Failed to send Forgejo PR mark-ready request")?;

        let status = resp.status();
        tracing::debug!(status = %status, "Forgejo PR mark-ready response");

        resp.error_for_status().context(format!(
            "Forgejo PR mark-ready failed with status {}",
            status
        ))?;

        tracing::debug!(pr_number = pr_number, "Forgejo PR marked as ready");
        Ok(())
    }

    fn provider_id(&self) -> &str {
        &self.forgejo_url
    }
}
