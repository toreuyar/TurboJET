//! Forgejo Actions-compatible external CI/deployment adapter.
//!
//! TurboJet only *requests* a workflow dispatch. Forgejo Actions keeps the
//! execution authority, credentials and deployment target. The workflow is
//! expected to call TurboJet's signed callback endpoint. Polling is a fallback
//! for reconciliation after a lost response or restart, not an execution path.

use crate::model::{CiState, ReleaseEnvironment, ReleaseKind};
use anyhow::{Context, Result as AnyResult};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct ForgejoActionsConfig {
    pub base_url: String,
    pub token: String,
    pub owner: String,
    pub repo: String,
    pub workflow: String,
    pub git_ref: String,
    pub callback_base_url: Option<String>,
}

impl ForgejoActionsConfig {
    pub fn validate(&self) -> AnyResult<()> {
        anyhow::ensure!(
            self.base_url.starts_with("http://") || self.base_url.starts_with("https://"),
            "delivery base_url must be http(s)"
        );
        anyhow::ensure!(!self.token.trim().is_empty(), "delivery token is empty");
        anyhow::ensure!(!self.owner.trim().is_empty(), "delivery owner is empty");
        anyhow::ensure!(!self.repo.trim().is_empty(), "delivery repo is empty");
        anyhow::ensure!(
            !self.workflow.trim().is_empty(),
            "delivery workflow is empty"
        );
        anyhow::ensure!(!self.git_ref.trim().is_empty(), "delivery git_ref is empty");
        anyhow::ensure!(
            !self.workflow.contains("..") && !self.workflow.contains('?'),
            "delivery workflow name is invalid"
        );
        if let Some(base) = &self.callback_base_url {
            anyhow::ensure!(
                base.starts_with("http://") || base.starts_with("https://"),
                "delivery callback_base_url must be http(s)"
            );
            anyhow::ensure!(
                !base.contains(['\r', '\n']),
                "callback_base_url must be a single URL"
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct DispatchRequest {
    pub release_id: String,
    pub run_id: String,
    pub environment: ReleaseEnvironment,
    pub kind: ReleaseKind,
    pub commit_sha: String,
    pub expected_revision: String,
    pub operation_key: String,
    pub callback_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DispatchReceipt {
    pub external_run_id: Option<String>,
    pub external_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExternalRun {
    pub external_run_id: String,
    pub status: CiState,
    pub commit_sha: Option<String>,
    pub external_url: Option<String>,
    pub workflow: Option<String>,
    pub detail: Option<String>,
    /// External creation timestamp when provided, used to pick the newest run
    /// for the exact commit instead of trusting API ordering.
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Error)]
#[error("delivery dispatch failed: {message}")]
pub struct DispatchFailure {
    pub message: String,
    /// True when a timeout/transport loss or 5xx means the external workflow may
    /// have started. The caller must record `unknown`, never `failed` or success.
    pub uncertain: bool,
}

impl DispatchFailure {
    fn definite(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            uncertain: false,
        }
    }
    fn uncertain(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            uncertain: true,
        }
    }
}

#[async_trait]
pub trait DeliveryAdapter: Send + Sync {
    fn provider_id(&self) -> &str;
    async fn dispatch(&self, request: &DispatchRequest)
    -> Result<DispatchReceipt, DispatchFailure>;
    async fn poll(&self, commit_sha: &str) -> Result<Vec<ExternalRun>, DispatchFailure>;
}

pub struct ForgejoActionsAdapter {
    config: ForgejoActionsConfig,
    client: Client,
}

impl ForgejoActionsAdapter {
    pub fn new(config: ForgejoActionsConfig) -> AnyResult<Self> {
        config.validate()?;
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .context("cannot build delivery HTTP client")?;
        Ok(Self { config, client })
    }

    fn api_base(&self) -> String {
        format!(
            "{}/api/v1/repos/{}/{}",
            self.config.base_url.trim_end_matches('/'),
            self.config.owner,
            self.config.repo
        )
    }
}

#[async_trait]
impl DeliveryAdapter for ForgejoActionsAdapter {
    fn provider_id(&self) -> &str {
        "forgejo_actions"
    }

    async fn dispatch(
        &self,
        request: &DispatchRequest,
    ) -> Result<DispatchReceipt, DispatchFailure> {
        let url = format!(
            "{}/actions/workflows/{}/dispatches",
            self.api_base(),
            request_workflow(&self.config.workflow)
        );
        let callback_url = self.config.callback_base_url.as_ref().map(|base| {
            format!(
                "{}{}",
                base.as_str().trim_end_matches('/'),
                request.callback_path
            )
        });
        let inputs = serde_json::json!({
            "turbojet_release_id": request.release_id,
            "turbojet_run_id": request.run_id,
            "turbojet_environment": request.environment.as_str(),
            "turbojet_kind": request.kind.as_str(),
            "turbojet_commit": request.commit_sha,
            "turbojet_expected_revision": request.expected_revision,
            "turbojet_operation_key": request.operation_key,
            "turbojet_callback_path": request.callback_path,
            "turbojet_callback_url": callback_url,
        });
        let body = serde_json::json!({
            "ref": self.config.git_ref,
            "inputs": inputs,
        });
        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("token {}", self.config.token))
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|error| {
                DispatchFailure::uncertain(format!("workflow dispatch transport failed: {error}"))
            })?;
        let status = response.status();
        if status.is_success() {
            let bytes = response.bytes().await.unwrap_or_default();
            let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            let external_run_id = value
                .get("id")
                .or_else(|| value.get("run_id"))
                .or_else(|| value.pointer("/workflow_run/id"))
                .and_then(value_to_string);
            let external_url = value
                .get("html_url")
                .or_else(|| value.get("web_url"))
                .or_else(|| value.pointer("/workflow_run/html_url"))
                .and_then(Value::as_str)
                .map(str::to_string);
            return Ok(DispatchReceipt {
                external_run_id,
                external_url,
            });
        }
        let message = response.text().await.unwrap_or_default();
        let failure = format!("workflow dispatch returned {status}: {message}");
        // 4xx means Forgejo refused the request before running a workflow. A
        // 5xx/timeout may have happened after the workflow was accepted.
        if status.is_client_error() {
            Err(DispatchFailure::definite(failure))
        } else {
            Err(DispatchFailure::uncertain(failure))
        }
    }

    async fn poll(&self, commit_sha: &str) -> Result<Vec<ExternalRun>, DispatchFailure> {
        let url = format!("{}/actions/runs?limit=50", self.api_base());
        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("token {}", self.config.token))
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|error| {
                DispatchFailure::uncertain(format!("workflow polling failed: {error}"))
            })?;
        let status = response.status();
        if !status.is_success() {
            let message = response.text().await.unwrap_or_default();
            return Err(DispatchFailure::uncertain(format!(
                "workflow polling returned {status}: {message}"
            )));
        }
        let body: Value = response.json().await.map_err(|error| {
            DispatchFailure::uncertain(format!("cannot parse workflow runs: {error}"))
        })?;
        let runs = body
            .get("workflow_runs")
            .or_else(|| body.get("runs"))
            .and_then(Value::as_array)
            .cloned()
            .or_else(|| body.as_array().cloned())
            .unwrap_or_default();
        let mut matching = Vec::new();
        for run in runs {
            let run_commit = run
                .get("head_sha")
                .or_else(|| run.get("commit_sha"))
                .or_else(|| run.get("sha"))
                .and_then(Value::as_str)
                .map(str::to_string);
            if run_commit.as_deref() != Some(commit_sha) {
                continue;
            }
            let raw_status = run
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let conclusion = run.get("conclusion").and_then(Value::as_str);
            let status = if raw_status == "completed" {
                conclusion.map(parse_conclusion).unwrap_or(CiState::Unknown)
            } else {
                CiState::parse(raw_status).unwrap_or(CiState::Unknown)
            };
            let external_run_id = run
                .get("id")
                .and_then(value_to_string)
                .or_else(|| run.get("run_id").and_then(value_to_string))
                .unwrap_or_else(|| {
                    format!("{}:{}", raw_status, run_commit.clone().unwrap_or_default())
                });
            matching.push(ExternalRun {
                external_run_id,
                status,
                commit_sha: run_commit,
                external_url: run
                    .get("html_url")
                    .or_else(|| run.get("web_url"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
                workflow: run
                    .get("name")
                    .or_else(|| run.get("workflow_name"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
                detail: run
                    .get("display_title")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                created_at: run
                    .get("created_at")
                    .or_else(|| run.get("started_at"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
            });
        }
        matching.sort_by(|a, b| {
            b.created_at.cmp(&a.created_at).then_with(|| {
                let left = a.external_run_id.parse::<u64>().unwrap_or(0);
                let right = b.external_run_id.parse::<u64>().unwrap_or(0);
                right.cmp(&left)
            })
        });
        Ok(matching)
    }
}

fn request_workflow(workflow: &str) -> String {
    // Forgejo accepts the workflow file name or the numeric workflow id. Keep
    // simple path-safe identifiers; the configured value is validated above.
    workflow.trim_start_matches('/').to_string()
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn parse_conclusion(value: &str) -> CiState {
    match value {
        "success" => CiState::Succeeded,
        "failure" | "timed_out" | "startup_failure" => CiState::Failed,
        "cancelled" | "canceled" | "skipped" => CiState::Cancelled,
        "neutral" | "stale" => CiState::Unknown,
        other => CiState::parse(other).unwrap_or(CiState::Unknown),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn dispatch_and_poll_against_disposable_http_forgejo_protocol() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::<String>::new()));
        let requests_for_server = Arc::clone(&requests);
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let requests = Arc::clone(&requests_for_server);
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 8192];
                    let header_end = loop {
                        let n = stream.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(index) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break index + 4;
                        }
                    };
                    let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
                    let request_line = headers.lines().next().unwrap_or_default().to_string();
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    let mut body = buf[header_end..].to_vec();
                    while body.len() < content_length {
                        let n = stream.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        body.extend_from_slice(&chunk[..n]);
                    }
                    requests.lock().unwrap().push(format!(
                        "{}\n{}",
                        request_line,
                        String::from_utf8_lossy(&body)
                    ));
                    if request_line.starts_with(
                        "POST /api/v1/repos/owner/repo/actions/workflows/delivery.yml/dispatches",
                    ) {
                        let body = br#"{"id":42,"html_url":"http://localhost/actions/42"}"#;
                        let head = format!(
                            "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(head.as_bytes()).await;
                        let _ = stream.write_all(body).await;
                    } else if request_line.starts_with("GET /api/v1/repos/owner/repo/actions/runs")
                    {
                        let body = serde_json::to_vec(&serde_json::json!({"workflow_runs":[
                            {"id":42,"status":"completed","conclusion":"success","head_sha":"abc",
                             "html_url":"http://localhost/actions/42","name":"delivery"},
                            {"id":41,"status":"completed","conclusion":"failure","head_sha":"other"}
                        ]}))
                        .unwrap();
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(head.as_bytes()).await;
                        let _ = stream.write_all(&body).await;
                    } else {
                        let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                    }
                });
            }
        });
        let adapter = ForgejoActionsAdapter::new(ForgejoActionsConfig {
            base_url: base,
            token: "test-token".into(),
            owner: "owner".into(),
            repo: "repo".into(),
            workflow: "delivery.yml".into(),
            git_ref: "main".into(),
            callback_base_url: Some("http://turbojet.local".into()),
        })
        .unwrap();
        let receipt = adapter
            .dispatch(&DispatchRequest {
                release_id: "rel".into(),
                run_id: "run".into(),
                environment: ReleaseEnvironment::Staging,
                kind: ReleaseKind::Deploy,
                commit_sha: "abc".into(),
                expected_revision: "abc".into(),
                operation_key: "op".into(),
                callback_path: "/v1/delivery/releases/rel/callbacks".into(),
            })
            .await
            .unwrap();
        assert_eq!(receipt.external_run_id.as_deref(), Some("42"));
        let runs = adapter.poll("abc").await.unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, CiState::Succeeded);
        assert_eq!(runs[0].external_run_id, "42");
        let recorded = requests.lock().unwrap().join("\n");
        assert!(recorded.contains(
            "\"turbojet_callback_url\":\"http://turbojet.local/v1/delivery/releases/rel/callbacks\""
        ));
        assert!(recorded.contains("\"ref\":\"main\""));
        server.abort();
    }
}
