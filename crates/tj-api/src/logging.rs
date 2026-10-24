use axum::extract::{MatchedPath, Request};
use axum::middleware::Next;
use axum::response::Response;
use tracing::Instrument;

/// Axum's rejection TRACE events include raw error/response bodies. Network
/// libraries can likewise trace raw URLs and protocol data. Keep those events
/// out of both diagnostic sinks even when RUST_LOG enables verbose tracing;
/// application request/provider metadata remains available.
pub fn diagnostic_event_allowed(metadata: &tracing::Metadata<'_>) -> bool {
    let target = metadata.target();
    ![
        "axum::rejection",
        "reqwest",
        "hyper",
        "hyper_util",
        "h2",
        "tower_http::trace",
    ]
    .iter()
    .any(|prefix| {
        target
            .strip_prefix(*prefix)
            .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with("::"))
    })
}

/// HTTP diagnostics are metadata-only, including when the legacy
/// TURBOJET_LOG_BODIES flag is set. Body/header/query filtering cannot reliably
/// identify credentials in free text, provider errors, or new API routes.
/// Detailed agent activity belongs in the authorized run activity store.
///
/// Never consume a request or response body here: extractors retain their body
/// limits/errors, and streaming responses reach clients without buffering.
pub async fn log_http(req: Request, next: Next) -> Response {
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|path| path.as_str().to_string())
        .unwrap_or_else(|| "<unmatched>".to_string());
    let request_id = uuid::Uuid::new_v4().to_string();
    let span = tracing::info_span!(
        "http_request",
        request_id = %request_id,
        method = %req.method(),
        route = %route,
    );

    async move {
        let start = std::time::Instant::now();
        tracing::debug!("HTTP request started");
        let mut response = next.run(req).await;
        let status = response.status();
        let response_headers_ms = start.elapsed().as_millis() as u64;
        if status.is_success() {
            tracing::info!(
                status = status.as_u16(),
                response_headers_ms,
                "HTTP response ready"
            );
        } else {
            tracing::warn!(
                status = status.as_u16(),
                response_headers_ms,
                "HTTP response ready"
            );
        }
        // Generated locally: never reflect an untrusted caller's identifier
        // into logs. The response ID lets an operator correlate a report.
        response.headers_mut().insert(
            "x-request-id",
            request_id.parse().expect("UUID is a valid header"),
        );
        response
    }
    .instrument(span)
    .await
}

#[cfg(test)]
mod tests;
