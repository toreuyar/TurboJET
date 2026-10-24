use super::*;
use crate::types::{Message, Role};
use axum::{Json, Router, http::StatusCode, routing::post};
use std::{
    io::Write,
    sync::{Arc, Mutex},
};
use tracing::instrument::WithSubscriber;

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Captured {
    fn subscriber(&self) -> impl tracing::Subscriber + Send + Sync + 'static {
        let writer = self.clone();
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish()
    }
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

struct MockProvider {
    url: String,
    task: tokio::task::JoinHandle<()>,
}

impl MockProvider {
    async fn start(status: StatusCode, body: serde_json::Value) -> Self {
        let app = Router::new().route(
            "/chat/completions",
            post(move || {
                let body = body.clone();
                async move { (status, Json(body)) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async {
            axum::serve(listener, app).await.unwrap();
        });
        Self { url, task }
    }
}

impl Drop for MockProvider {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn request() -> ChatRequest {
    ChatRequest {
        model: "test-model".into(),
        messages: vec![Message {
            role: Role::User,
            content: "sentinel-prompt-secret".into(),
        }],
        tools: None,
        max_tokens: None,
        temperature: None,
    }
}

#[tokio::test]
async fn successful_chat_preserves_content_but_does_not_log_payloads_or_key_prefix() {
    let server = MockProvider::start(
        StatusCode::OK,
        serde_json::json!({
            "choices": [{"message": {"content": "sentinel-response-secret"}}],
            "usage": {"prompt_tokens": 2, "completion_tokens": 3},
        }),
    )
    .await;
    let provider = OpenAiProvider::new(
        "test-provider",
        &server.url,
        "sentinel-api-key",
        "test-model",
    );
    let captured = Captured::default();
    let response = provider
        .chat(request())
        .with_subscriber(captured.subscriber())
        .await
        .unwrap();
    assert_eq!(response.content, "sentinel-response-secret");
    assert_eq!(response.usage.input, 2);
    assert!(response.usage.reported);
    let logs = captured.text();
    assert!(logs.contains("Chat response received"));
    assert!(
        !logs.contains("sentinel"),
        "provider logged sensitive data: {logs}"
    );
}

#[tokio::test]
async fn provider_errors_never_propagate_echoed_secrets_in_chat_or_stream() {
    let server = MockProvider::start(
        StatusCode::UNAUTHORIZED,
        serde_json::json!({
            "error": {"message": "sentinel-api-key sentinel-prompt-secret"},
        }),
    )
    .await;
    let provider = OpenAiProvider::new(
        "test-provider",
        &server.url,
        "sentinel-api-key",
        "test-model",
    );
    let captured = Captured::default();
    async {
        let error = provider.chat(request()).await.unwrap_err();
        assert!(error.to_string().contains("401"));
        assert!(!format!("{error:#}").contains("sentinel"));
        let mut stream = provider.chat_stream(request());
        let error = stream.next().await.unwrap().unwrap_err();
        assert!(error.to_string().contains("401"));
        assert!(!format!("{error:#}").contains("sentinel"));
        assert!(stream.next().await.is_none());
    }
    .with_subscriber(captured.subscriber())
    .await;
    assert!(!captured.text().contains("sentinel"));
}

#[tokio::test]
async fn missing_provider_usage_is_marked_unknown_not_zero() {
    let server = MockProvider::start(
        StatusCode::OK,
        serde_json::json!({
            "choices": [{"message": {"content": "ok"}}],
        }),
    )
    .await;
    let provider = OpenAiProvider::new("test-provider", &server.url, "unused", "test-model");
    let response = provider.chat(request()).await.unwrap();
    assert!(!response.usage.reported);
    assert_eq!(response.usage.input, 0);
    assert_eq!(response.usage.output, 0);
}

#[tokio::test]
async fn failed_transport_does_not_expose_url_credentials_in_propagated_errors() {
    // An unsupported URL scheme deterministically fails before any network I/O.
    let provider = OpenAiProvider::new(
        "test-provider",
        "unsupported://sentinel-user:sentinel-password@localhost/sentinel-path?token=sentinel-token",
        "sentinel-api-key",
        "test-model",
    );
    let captured = Captured::default();
    async {
        let error = provider.chat(request()).await.unwrap_err();
        assert!(!format!("{error:#}").contains("sentinel"));
        let mut stream = provider.chat_stream(request());
        let error = stream.next().await.unwrap().unwrap_err();
        assert!(!format!("{error:#}").contains("sentinel"));
    }
    .with_subscriber(captured.subscriber())
    .await;
    assert!(!captured.text().contains("sentinel"));
}
