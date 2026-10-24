//! Bounded provider retries with explicit failure classes (TJ-024).
//!
//! Retries happen at the *model transport* boundary only. Nothing here invokes
//! tools, so a transport retry can never replay a tool call or other side
//! effect. Rate-limit and transient failures have separate attempt budgets;
//! credential failures and ordinary request errors are never retried.

use adk_core::{AdkError, Llm, LlmRequest, LlmResponse, LlmResponseStream};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderErrorClass {
    RateLimit,
    Transient,
    Credential,
    Permanent,
}

impl ProviderErrorClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RateLimit => "rate_limit",
            Self::Transient => "transient",
            Self::Credential => "credential",
            Self::Permanent => "permanent",
        }
    }

    pub fn retriable(self) -> bool {
        matches!(self, Self::RateLimit | Self::Transient)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderFailure {
    pub class: ProviderErrorClass,
    pub http_status: Option<i64>,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResiliencePolicy {
    pub rate_limit_retries: u32,
    pub transient_retries: u32,
    pub base_backoff_ms: u64,
}

impl Default for ResiliencePolicy {
    fn default() -> Self {
        Self {
            rate_limit_retries: 3,
            transient_retries: 2,
            base_backoff_ms: 250,
        }
    }
}

impl ResiliencePolicy {
    pub fn from_provider(provider: &tj_config::system::ProviderConfig) -> Self {
        Self {
            rate_limit_retries: provider.rate_limit_retries,
            transient_retries: provider.transient_retries,
            base_backoff_ms: provider.retry_base_ms,
        }
    }

    pub(crate) fn max_for(&self, class: ProviderErrorClass) -> u32 {
        match class {
            ProviderErrorClass::RateLimit => self.rate_limit_retries,
            ProviderErrorClass::Transient => self.transient_retries,
            ProviderErrorClass::Credential | ProviderErrorClass::Permanent => 0,
        }
    }

    pub(crate) fn backoff(&self, retry_index: u32) -> Duration {
        let factor = 1u64 << retry_index.min(6);
        Duration::from_millis(self.base_backoff_ms.saturating_mul(factor))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderAttemptRecord {
    pub provider: String,
    pub model: String,
    pub outcome: String,
    pub error_class: Option<String>,
    pub http_status: Option<i64>,
    pub retry_index: u32,
    pub duration_ms: u64,
    pub detail: Option<String>,
}

/// Observer implemented by the runtime to persist each bounded attempt. The
/// detail is a short, already-redacted classification message, never a raw
/// provider body (those may echo credentials or prompts).
pub trait ProviderObserver: Send + Sync {
    fn record(&self, attempt: &ProviderAttemptRecord);
}

pub struct ResilientLlm {
    inner: Arc<dyn Llm>,
    policy: ResiliencePolicy,
    provider_id: String,
    observer: Option<Arc<dyn ProviderObserver>>,
}

impl ResilientLlm {
    pub fn new(
        inner: Arc<dyn Llm>,
        provider_id: impl Into<String>,
        policy: ResiliencePolicy,
        observer: Option<Arc<dyn ProviderObserver>>,
    ) -> Self {
        Self {
            inner,
            policy,
            provider_id: provider_id.into(),
            observer,
        }
    }

    fn observe(
        &self,
        model: &str,
        outcome: &str,
        failure: Option<&ProviderFailure>,
        retry_index: u32,
        elapsed: Duration,
    ) {
        if let Some(observer) = &self.observer {
            observer.record(&ProviderAttemptRecord {
                provider: self.provider_id.clone(),
                model: model.to_string(),
                outcome: outcome.to_string(),
                error_class: failure.map(|f| f.class.as_str().to_string()),
                http_status: failure.and_then(|f| f.http_status),
                retry_index,
                duration_ms: elapsed.as_millis().try_into().unwrap_or(u64::MAX),
                detail: failure.map(|f| f.message.chars().take(200).collect()),
            });
        }
    }
}

#[async_trait]
impl Llm for ResilientLlm {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn generate_content(
        &self,
        req: LlmRequest,
        stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        let model = req.model.clone();
        let mut rate_used = 0u32;
        let mut transient_used = 0u32;
        let mut retry_index = 0u32;
        loop {
            let started = Instant::now();
            match self.inner.generate_content(req.clone(), stream).await {
                Ok(mut response) => {
                    use futures::StreamExt;
                    match response.next().await {
                        Some(Err(error)) => {
                            let failure = classify_adk_error(&error);
                            let used = match failure.class {
                                ProviderErrorClass::RateLimit => &mut rate_used,
                                ProviderErrorClass::Transient => &mut transient_used,
                                _ => {
                                    self.observe(
                                        &model,
                                        "failed",
                                        Some(&failure),
                                        retry_index,
                                        started.elapsed(),
                                    );
                                    return Ok(Box::pin(futures::stream::iter([Err(error)])));
                                }
                            };
                            let max = self.policy.max_for(failure.class);
                            self.observe(
                                &model,
                                "retryable",
                                Some(&failure),
                                retry_index,
                                started.elapsed(),
                            );
                            if *used < max {
                                let delay = self.policy.backoff(retry_index);
                                *used += 1;
                                retry_index += 1;
                                tracing::warn!(
                                    provider = %self.provider_id,
                                    class = failure.class.as_str(),
                                    retry = *used,
                                    max,
                                    delay_ms = delay.as_millis() as u64,
                                    "Provider stream error retried before content; no tool replay"
                                );
                                tokio::time::sleep(delay).await;
                                continue;
                            }
                            return Ok(Box::pin(futures::stream::iter([Err(error)])));
                        }
                        Some(Ok(item)) if is_error_response(&item) => {
                            let message = format!(
                                "{} {}",
                                item.error_code.clone().unwrap_or_default(),
                                item.error_message.clone().unwrap_or_default()
                            );
                            let failure = classify_error(&message);
                            let used = match failure.class {
                                ProviderErrorClass::RateLimit => &mut rate_used,
                                ProviderErrorClass::Transient => &mut transient_used,
                                _ => {
                                    self.observe(
                                        &model,
                                        "failed",
                                        Some(&failure),
                                        retry_index,
                                        started.elapsed(),
                                    );
                                    let mut rest = response;
                                    return Ok(Box::pin(async_stream::stream! {
                                        yield Ok(item);
                                        while let Some(next) = rest.next().await { yield next; }
                                    }));
                                }
                            };
                            let max = self.policy.max_for(failure.class);
                            self.observe(
                                &model,
                                "retryable",
                                Some(&failure),
                                retry_index,
                                started.elapsed(),
                            );
                            if *used < max {
                                let delay = self.policy.backoff(retry_index);
                                *used += 1;
                                retry_index += 1;
                                tracing::warn!(
                                    provider = %self.provider_id,
                                    class = failure.class.as_str(),
                                    retry = *used,
                                    max,
                                    delay_ms = delay.as_millis() as u64,
                                    "Provider stream error retried before any content; no tool replay"
                                );
                                tokio::time::sleep(delay).await;
                                continue;
                            }
                            let mut rest = response;
                            return Ok(Box::pin(async_stream::stream! {
                                yield Ok(item);
                                while let Some(next) = rest.next().await { yield next; }
                            }));
                        }
                        Some(item) => {
                            self.observe(&model, "success", None, retry_index, started.elapsed());
                            let mut rest = response;
                            return Ok(Box::pin(async_stream::stream! {
                                yield item;
                                while let Some(next) = rest.next().await { yield next; }
                            }));
                        }
                        None => {
                            self.observe(&model, "success", None, retry_index, started.elapsed());
                            return Ok(Box::pin(futures::stream::empty()));
                        }
                    }
                }
                Err(error) => {
                    let failure = classify_adk_error(&error);
                    let used = match failure.class {
                        ProviderErrorClass::RateLimit => &mut rate_used,
                        ProviderErrorClass::Transient => &mut transient_used,
                        _ => {
                            self.observe(
                                &model,
                                "failed",
                                Some(&failure),
                                retry_index,
                                started.elapsed(),
                            );
                            return Err(AdkError::model(format!(
                                "provider {} {} failure (credential/request errors are not retried): {}",
                                self.provider_id,
                                failure.class.as_str(),
                                error
                            )));
                        }
                    };
                    let max = self.policy.max_for(failure.class);
                    self.observe(
                        &model,
                        "retryable",
                        Some(&failure),
                        retry_index,
                        started.elapsed(),
                    );
                    if *used >= max {
                        return Err(AdkError::model(format!(
                            "provider {} {} retry budget exhausted ({}/{}): {}",
                            self.provider_id,
                            failure.class.as_str(),
                            *used,
                            max,
                            error
                        )));
                    }
                    let delay = self.policy.backoff(retry_index);
                    *used += 1;
                    retry_index += 1;
                    tracing::warn!(
                        provider = %self.provider_id,
                        class = failure.class.as_str(),
                        retry = *used,
                        max,
                        delay_ms = delay.as_millis() as u64,
                        "Provider transport retry; no tool was or will be replayed for this retry"
                    );
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }
}

fn is_error_response(response: &LlmResponse) -> bool {
    response.content.is_none()
        && (response
            .error_code
            .as_deref()
            .is_some_and(|code| !code.is_empty())
            || response
                .error_message
                .as_deref()
                .is_some_and(|message| !message.is_empty()))
}

pub fn classify_adk_error(error: &AdkError) -> ProviderFailure {
    let mut failure = classify_error(&error.to_string());
    if error.is_unauthorized() {
        failure.class = ProviderErrorClass::Credential;
    } else if error.is_rate_limited() {
        failure.class = ProviderErrorClass::RateLimit;
    } else if (error.is_timeout() || error.is_retryable())
        && failure.class == ProviderErrorClass::Permanent
    {
        failure.class = ProviderErrorClass::Transient;
    }
    failure
}

pub fn classify_error(message: &str) -> ProviderFailure {
    let lower = message.to_ascii_lowercase();
    let http_status = extract_status(&lower);
    let credential = http_status == Some(401)
        || http_status == Some(403)
        || lower.contains("unauthorized")
        || lower.contains("forbidden")
        || lower.contains("invalid api key")
        || lower.contains("incorrect api key")
        || lower.contains("authentication")
        || lower.contains("credential")
        || lower.contains("permission denied");
    let class = if credential {
        ProviderErrorClass::Credential
    } else if http_status == Some(429)
        || lower.contains("rate limit")
        || lower.contains("too many requests")
        || lower.contains("quota exceeded")
    {
        ProviderErrorClass::RateLimit
    } else if http_status.is_some_and(|status| status == 408 || status >= 500)
        || lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("connection reset")
        || lower.contains("connection refused")
        || lower.contains("connection closed")
        || lower.contains("broken pipe")
        || lower.contains("temporarily unavailable")
        || lower.contains("service unavailable")
        || lower.contains("overloaded")
        || lower.contains("network error")
        || lower.contains("error sending request")
    {
        ProviderErrorClass::Transient
    } else {
        ProviderErrorClass::Permanent
    };
    ProviderFailure {
        class,
        http_status,
        message: message.to_string(),
    }
}

fn extract_status(lower: &str) -> Option<i64> {
    let bytes = lower.as_bytes();
    for status in [
        400i64, 401, 403, 404, 408, 409, 422, 429, 500, 502, 503, 504,
    ] {
        let needle = status.to_string();
        let mut start = 0;
        while let Some(pos) = lower[start..].find(&needle) {
            let absolute = start + pos;
            let before_ok = absolute == 0 || !bytes[absolute - 1].is_ascii_digit();
            let end = absolute + needle.len();
            let after_ok = end >= bytes.len() || !bytes[end].is_ascii_digit();
            if before_ok && after_ok {
                return Some(status);
            }
            start = end;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn separates_rate_limit_transient_and_credential_errors() {
        assert_eq!(
            classify_error("HTTP 429 Too Many Requests").class,
            ProviderErrorClass::RateLimit
        );
        assert_eq!(
            classify_error("rate limit exceeded").class,
            ProviderErrorClass::RateLimit
        );
        assert_eq!(
            classify_error("HTTP 503 service unavailable").class,
            ProviderErrorClass::Transient
        );
        assert_eq!(
            classify_error("connection reset by peer").class,
            ProviderErrorClass::Transient
        );
        assert_eq!(
            classify_error("HTTP 401 unauthorized").class,
            ProviderErrorClass::Credential
        );
        assert_eq!(
            classify_error("invalid api key").class,
            ProviderErrorClass::Credential
        );
        assert_eq!(
            classify_error("HTTP 400 malformed request").class,
            ProviderErrorClass::Permanent
        );
        assert!(!ProviderErrorClass::Credential.retriable());
        assert!(ProviderErrorClass::Transient.retriable());
    }

    struct FlakyLlm {
        failures: Mutex<Vec<&'static str>>,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl Llm for FlakyLlm {
        fn name(&self) -> &str {
            "flaky"
        }
        async fn generate_content(
            &self,
            _req: LlmRequest,
            _stream: bool,
        ) -> adk_core::Result<LlmResponseStream> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let next = self.failures.lock().unwrap().pop();
            match next {
                Some(message) => Err(adk_core::AdkError::model(message.to_string())),
                None => Ok(Box::pin(futures::stream::empty())),
            }
        }
    }

    struct Counter(AtomicUsize);
    impl ProviderObserver for Counter {
        fn record(&self, _attempt: &ProviderAttemptRecord) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn retries_are_bounded_per_class_and_credentials_never_retry() {
        let flaky = Arc::new(FlakyLlm {
            failures: Mutex::new(vec![
                "rate limit exceeded",
                "rate limit exceeded",
                "rate limit exceeded",
            ]),
            calls: AtomicUsize::new(0),
        });
        let observer = Arc::new(Counter(AtomicUsize::new(0)));
        let llm = ResilientLlm::new(
            flaky.clone(),
            "p",
            ResiliencePolicy {
                rate_limit_retries: 2,
                transient_retries: 1,
                base_backoff_ms: 1,
            },
            Some(Arc::clone(&observer) as Arc<dyn ProviderObserver>),
        );
        let err = match llm
            .generate_content(LlmRequest::new("m", vec![]), false)
            .await
        {
            Ok(_) => panic!("expected retry exhaustion"),
            Err(error) => error,
        };
        assert!(err.to_string().contains("retry budget exhausted"));
        assert_eq!(flaky.calls.load(Ordering::SeqCst), 3);
        assert_eq!(observer.0.load(Ordering::SeqCst), 3);

        let credential = Arc::new(FlakyLlm {
            failures: Mutex::new(vec!["HTTP 401 unauthorized"]),
            calls: AtomicUsize::new(0),
        });
        let llm = ResilientLlm::new(credential.clone(), "p", ResiliencePolicy::default(), None);
        assert!(
            llm.generate_content(LlmRequest::new("m", vec![]), false)
                .await
                .is_err()
        );
        assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
    }
}
