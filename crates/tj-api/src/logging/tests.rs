use super::log_http;
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Request},
    http::{StatusCode, header},
    middleware,
    response::{IntoResponse, Sse, sse::Event},
    routing::{any, get, post},
};
use http_body_util::BodyExt;
use std::{
    io::Write,
    sync::{Arc, Mutex},
    time::Duration,
};
use tower::ServiceExt;
use tracing::instrument::WithSubscriber;
use tracing_subscriber::layer::SubscriberExt;

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
            .with(tracing_subscriber::filter::filter_fn(
                super::diagnostic_event_allowed,
            ))
    }
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

#[tokio::test]
async fn diagnostics_exclude_credentials_bodies_headers_and_raw_urls() {
    async fn echo(req: Request) -> impl IntoResponse {
        (
            [(header::SET_COOKIE, "session=sentinel-response-cookie")],
            req.into_body(),
        )
    }
    let app = Router::new()
        .route("/v1/auth/login", any(echo))
        .route("/v1/users", any(echo))
        .route("/v1/projects/{project_id}/secrets/{key}", any(echo))
        .route("/v1/projects/{project_id}/ssh-key", any(echo))
        .layer(middleware::from_fn(log_http));
    let captured = Captured::default();
    async {
        for (method, path) in [
            ("POST", "/v1/auth/login"),
            ("POST", "/v1/users"),
            ("PUT", "/v1/projects/sentinel-project/secrets/sentinel-key"),
            ("GET", "/v1/projects/sentinel-project/ssh-key"),
        ] {
            let payload = r#"{"password":"sentinel-password","value":"sentinel-secret","private_key":"sentinel-private-key","message":"sentinel-free-text"}"#;
            let request = Request::builder().method(method)
                .uri(format!("{path}?token=sentinel-query&token=sentinel-duplicate&%74oken=sentinel-encoded"))
                .header(header::AUTHORIZATION, "Bearer sentinel-authorization")
                .header(header::COOKIE, "session=sentinel-cookie")
                .header("x-api-key", "sentinel-api-key")
                .header("x-request-id", "sentinel-client-id")
                .body(Body::from(payload)).unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()[header::SET_COOKIE], "session=sentinel-response-cookie");
            let id = response.headers()["x-request-id"].to_str().unwrap().to_string();
            uuid::Uuid::parse_str(&id).unwrap();
            assert_eq!(axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap(), payload);
            assert!(captured.text().contains(&id));
        }
        let response = app.oneshot(Request::builder()
            .uri("/sentinel-unknown-path?token=sentinel-unmatched")
            .body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }.with_subscriber(captured.subscriber()).await;

    let logs = captured.text();
    assert!(
        !logs.contains("sentinel"),
        "sensitive request/response data logged: {logs}"
    );
    assert!(logs.contains("/v1/projects/{project_id}/secrets/{key}"));
    assert!(logs.contains("<unmatched>"));
    assert!(logs.contains("status=200"));
    assert!(logs.contains("status=404"));
    assert!(logs.contains("response_headers_ms="));
}

#[tokio::test]
async fn nonterminating_sse_reaches_client_incrementally() {
    let captured = Captured::default();
    async {
        let app = Router::new().route("/events", get(|| async {
            Sse::new(async_stream::stream! {
                yield Ok::<_, std::convert::Infallible>(Event::default().data("sentinel-event"));
                std::future::pending::<()>().await;
            })
        })).layer(middleware::from_fn(log_http));
        let response = tokio::time::timeout(
            Duration::from_secs(1),
            app.oneshot(
                Request::builder()
                    .uri("/events?token=sentinel-token")
                    .body(Body::empty())
                    .unwrap(),
            ),
        )
        .await
        .expect("middleware must return headers before stream ends")
        .unwrap();
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/event-stream"
        );
        let mut body = response.into_body();
        let frame = tokio::time::timeout(Duration::from_secs(1), body.frame())
            .await
            .expect("first event must arrive before stream ends")
            .unwrap()
            .unwrap();
        assert!(String::from_utf8_lossy(frame.data_ref().unwrap()).contains("sentinel-event"));
    }
    .with_subscriber(captured.subscriber())
    .await;
    assert!(!captured.text().contains("sentinel"), "{}", captured.text());
}

#[tokio::test]
async fn oversized_and_broken_request_bodies_are_rejected_not_replaced() {
    let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let handler_called = called.clone();
    let app = Router::new()
        .route(
            "/json",
            post(move |Json(value): Json<serde_json::Value>| async move {
                handler_called.store(true, std::sync::atomic::Ordering::SeqCst);
                Json(value)
            }),
        )
        .layer(DefaultBodyLimit::max(64))
        .layer(middleware::from_fn(log_http));
    let captured = Captured::default();
    async {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/json")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(format!(
                        "{{\"password\":\"sentinel-{}\"}}",
                        "x".repeat(128)
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

        let broken = Body::from_stream(futures::stream::iter([Err::<axum::body::Bytes, _>(
            std::io::Error::other("sentinel-body-read-error"),
        )]));
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/json")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(broken)
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
    }
    .with_subscriber(captured.subscriber())
    .await;
    assert!(!captured.text().contains("sentinel"), "{}", captured.text());
}
