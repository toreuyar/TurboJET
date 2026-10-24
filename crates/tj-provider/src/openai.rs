use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::StreamExt;
use reqwest::Client;
use tj_config::system::ProviderConfig;

use crate::provider::{ChatStream, LlmProvider};
use crate::types::{ChatRequest, ChatResponse, ModelInfo, TokenUsage};

pub struct OpenAiProvider {
    id: String,
    base_url: String,
    api_key: String,
    default_model: String,
    client: Client,
    policy: crate::resilience::ResiliencePolicy,
}

impl OpenAiProvider {
    pub fn from_config(config: &ProviderConfig) -> Result<Self> {
        let api_key = std::env::var(&config.api_key_env)
            .with_context(|| format!("Environment variable {} not set", config.api_key_env))?;
        Ok(Self {
            id: config.id.clone(),
            base_url: if config.base_url.is_empty() {
                "https://api.openai.com/v1".into()
            } else {
                config.base_url.clone()
            },
            api_key,
            default_model: config.default_model.clone(),
            client: Client::new(),
            policy: crate::resilience::ResiliencePolicy::from_provider(config),
        })
    }

    pub fn new(id: &str, base_url: &str, api_key: &str, default_model: &str) -> Self {
        Self {
            id: id.into(),
            base_url: base_url.into(),
            api_key: api_key.into(),
            default_model: default_model.into(),
            client: Client::new(),
            policy: crate::resilience::ResiliencePolicy::default(),
        }
    }

    fn parse_sse_token(line: &str) -> Option<String> {
        let data = line.strip_prefix("data: ")?;
        if data == "[DONE]" {
            return None;
        }
        let parsed: serde_json::Value = serde_json::from_str(data).ok()?;
        parsed
            .get("choices")?
            .get(0)?
            .get("delta")?
            .get("content")?
            .as_str()
            .map(|s| s.to_string())
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = serde_json::json!({
            "model": request.model,
            "messages": request.messages,
            "max_tokens": request.max_tokens,
            "temperature": request.temperature,
            "tools": request.tools,
        });
        tracing::debug!(
            provider = %self.id,
            message_count = request.messages.len(),
            "Sending chat request"
        );

        let start = std::time::Instant::now();

        // Transport retries are bounded per failure class and happen before any
        // response/tool is consumed. A retry can never replay a tool call.
        let mut rate_used = 0u32;
        let mut transient_used = 0u32;
        let mut retry_index = 0u32;
        let resp = loop {
            let sent = self
                .client
                .post(&url)
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await;
            match sent {
                Ok(resp) if resp.status().is_success() => break resp,
                Ok(resp) => {
                    let status = resp.status();
                    let failure = crate::resilience::classify_error(&format!(
                        "Provider {} returned HTTP {}",
                        self.id,
                        status.as_u16()
                    ));
                    let used = match failure.class {
                        crate::resilience::ProviderErrorClass::RateLimit => &mut rate_used,
                        crate::resilience::ProviderErrorClass::Transient => &mut transient_used,
                        _ => {
                            tracing::error!(provider = %self.id, status = %status, class = failure.class.as_str(), "Provider returned non-retryable error");
                            return Err(anyhow::anyhow!(
                                "Provider {} returned HTTP {}",
                                self.id,
                                status
                            ));
                        }
                    };
                    let max = self.policy.max_for(failure.class);
                    if *used >= max {
                        tracing::error!(provider = %self.id, status = %status, class = failure.class.as_str(), "Provider retry budget exhausted");
                        return Err(anyhow::anyhow!(
                            "Provider {} returned HTTP {} ({} retry budget exhausted {}/{})",
                            self.id,
                            status,
                            failure.class.as_str(),
                            *used,
                            max
                        ));
                    }
                    let delay = self.policy.backoff(retry_index);
                    *used += 1;
                    retry_index += 1;
                    tracing::warn!(provider = %self.id, status = %status, class = failure.class.as_str(), retry = *used, max, "Retrying provider transport request");
                    tokio::time::sleep(delay).await;
                }
                Err(error) => {
                    let error = error.without_url();
                    let failure = crate::resilience::classify_error(&error.to_string());
                    let used = match failure.class {
                        crate::resilience::ProviderErrorClass::RateLimit => &mut rate_used,
                        crate::resilience::ProviderErrorClass::Transient => &mut transient_used,
                        _ => {
                            return Err(error)
                                .context("Failed to send request to OpenAI-compatible provider");
                        }
                    };
                    let max = self.policy.max_for(failure.class);
                    if *used >= max {
                        return Err(error).context("Provider transport retry budget exhausted");
                    }
                    let delay = self.policy.backoff(retry_index);
                    *used += 1;
                    retry_index += 1;
                    tracing::warn!(provider = %self.id, class = failure.class.as_str(), retry = *used, max, "Retrying provider transport request");
                    tokio::time::sleep(delay).await;
                }
            }
        };

        let elapsed = start.elapsed();
        if elapsed.as_secs() >= 1 {
            tracing::warn!(elapsed_ms = elapsed.as_millis() as u64, operation = "llm_chat_request", provider = %self.id, "Slow LLM operation");
        } else {
            tracing::debug!(elapsed_ms = elapsed.as_millis() as u64, operation = "llm_chat_request", provider = %self.id, "LLM operation completed");
        }

        let body: serde_json::Value = resp.json().await.map_err(reqwest::Error::without_url)?;
        let choice = body
            .get("choices")
            .and_then(|c| c.get(0))
            .context("No choices in response")?;
        let content = choice
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();

        let tool_calls = choice
            .get("message")
            .and_then(|m| m.get("tool_calls"))
            .and_then(|tc| tc.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|tc| {
                        let raw_args = tc
                            .get("function")?
                            .get("arguments")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        let arguments = match raw_args {
                            serde_json::Value::String(s) => serde_json::from_str(&s)
                                .unwrap_or(serde_json::Value::Object(Default::default())),
                            other => other,
                        };
                        Some(crate::types::ToolCall {
                            id: tc.get("id")?.as_str()?.to_string(),
                            name: tc.get("function")?.get("name")?.as_str()?.to_string(),
                            arguments,
                        })
                    })
                    .collect::<Vec<_>>()
            });

        let usage = body
            .get("usage")
            .map(|u| TokenUsage {
                input: u.get("prompt_tokens").and_then(|t| t.as_u64()).unwrap_or(0) as u32,
                output: u
                    .get("completion_tokens")
                    .and_then(|t| t.as_u64())
                    .unwrap_or(0) as u32,
                reported: u.get("prompt_tokens").is_some() || u.get("completion_tokens").is_some(),
            })
            .unwrap_or_else(TokenUsage::unknown);

        let has_tools = tool_calls.as_ref().is_some_and(|t| !t.is_empty());
        tracing::debug!(
            provider = %self.id,
            model = %request.model,
            input_tokens = usage.input,
            output_tokens = usage.output,
            has_tool_calls = has_tools,
            "Chat response received"
        );

        Ok(ChatResponse {
            content,
            tool_calls,
            usage,
        })
    }

    fn chat_stream(&self, request: ChatRequest) -> ChatStream {
        let url = format!("{}/chat/completions", self.base_url);
        let api_key = self.api_key.clone();
        let client = self.client.clone();
        let provider_id = self.id.clone();
        let model = request.model.clone();

        Box::pin(async_stream::stream! {
            tracing::debug!(provider = %provider_id, model = %model, "Starting stream request");

            let resp = client
                .post(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .header("Content-Type", "application/json")
                .json(&serde_json::json!({
                    "model": request.model,
                    "messages": request.messages,
                    "max_tokens": request.max_tokens,
                    "temperature": request.temperature,
                    "stream": true,
                }))
                .send()
                .await;

            let resp = match resp {
                Ok(r) => r,
                Err(e) => {
                    yield Err(anyhow::anyhow!("Stream request failed: {}", e.without_url()));
                    return;
                }
            };

            if !resp.status().is_success() {
                let status = resp.status();
                tracing::error!(provider = %provider_id, status = %status, "Stream request failed");
                yield Err(anyhow::anyhow!("Provider {} returned HTTP {}", provider_id, status));
                return;
            }

            let mut stream = resp.bytes_stream();
            let mut buffer = String::new();

            while let Some(chunk_result) = stream.next().await {
                let chunk = match chunk_result {
                    Ok(c) => c,
                    Err(e) => {
                        yield Err(anyhow::anyhow!("Stream read error: {}", e.without_url()));
                        return;
                    }
                };

                buffer.push_str(&String::from_utf8_lossy(&chunk));

                while let Some(pos) = buffer.find('\n') {
                    let line = buffer[..pos].trim().to_string();
                    buffer = buffer[pos + 1..].to_string();

                    if line.is_empty() {
                        continue;
                    }

                    if let Some(token) = Self::parse_sse_token(&line) {
                        yield Ok(token);
                    }
                }
            }
        })
    }

    fn list_models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo {
            id: self.default_model.clone(),
            name: self.default_model.clone(),
        }]
    }

    fn provider_id(&self) -> &str {
        &self.id
    }
}

#[cfg(test)]
mod tests;
