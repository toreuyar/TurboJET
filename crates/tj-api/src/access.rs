use crate::{
    auth::{bearer_token, require_auth},
    handlers::AppState,
};
use axum::{
    Json,
    extract::{FromRequestParts, MatchedPath, Path, Query, State},
    http::{Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::{collections::HashMap, sync::Arc};

/// Runs after route matching; parameters are decoded by Axum, not hand-parsed.
pub async fn authorize(
    State(db): State<Arc<tj_db::Database>>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let (mut parts, body) = request.into_parts();
    let route = parts
        .extensions
        .get::<MatchedPath>()
        .map(|p| p.as_str().to_owned())
        .unwrap_or_default();
    if matches!(
        route.as_str(),
        "/" | "/api" | "/api/health" | "/api/ready" | "/v1/auth/login" | "/v1/auth/register"
    ) || route == "/v1/delivery/releases/{release_id}/callbacks"
    {
        return next
            .run(axum::extract::Request::from_parts(parts, body))
            .await;
    }
    let streaming = route == "/v1/pipeline/events" || route.ends_with("/stage-logs/stream");
    let token = if streaming {
        Query::<HashMap<String, String>>::from_request_parts(&mut parts, &())
            .await
            .ok()
            .and_then(|q| q.0.get("token").cloned())
    } else {
        bearer_token(&parts.headers)
    };
    let user = match token.and_then(|token| db.user_for_token(&token).ok().flatten()) {
        Some(user) => user,
        None if streaming => return super::handlers::sse_unauthorized(),
        None => return denied(StatusCode::UNAUTHORIZED),
    };
    let params = Path::<HashMap<String, String>>::from_request_parts(&mut parts, &())
        .await
        .map(|p| p.0)
        .unwrap_or_default();
    let allowed = if route.starts_with("/api/logs/")
        || route == "/v1/users"
        || route == "/v1/workflows/import"
        || route == "/v1/admin/activity/prune"
        || route.starts_with("/v1/projects/{project_id}/")
        || route == "/v1/lanes/{id}/workflow"
        || (route == "/v1/projects" && parts.method == Method::POST)
    {
        db.is_admin_user(&user.id)
    } else if route.starts_with("/v1/pipeline/runs/{id}") {
        db.can_access_run(
            &user.id,
            params.get("id").map(String::as_str).unwrap_or_default(),
        )
    } else if route.starts_with("/v1/pipeline/artifacts/{artifact_id}") {
        db.can_access_artifact(
            &user.id,
            params
                .get("artifact_id")
                .map(String::as_str)
                .unwrap_or_default(),
        )
    } else {
        // Collection handlers filter their results; creation checks its body.
        Ok(matches!(
            route.as_str(),
            "/v1/chat/completions"
                | "/v1/chats/{chat_id}"
                | "/v1/models"
                | "/v1/pipeline/runs"
                | "/v1/projects"
                | "/v1/lanes"
                | "/v1/workflows"
                | "/v1/workflows/{id}"
                | "/v1/profiles"
                | "/v1/pipeline/events"
                | "/v1/me"
                | "/v1/readiness"
                | "/v1/auth/logout"
        ))
    };
    match allowed {
        Ok(true) => {
            next.run(axum::extract::Request::from_parts(parts, body))
                .await
        }
        Ok(false) => denied(StatusCode::FORBIDDEN),
        Err(_) => denied(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

fn denied(status: StatusCode) -> Response {
    (status, Json(serde_json::json!({"error": "access denied"}))).into_response()
}

pub fn pipeline_events(
    db: Arc<tj_db::Database>,
    token: String,
    mut rx: tokio::sync::broadcast::Receiver<tj_db::PipelineNotification>,
) -> impl tokio_stream::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>
{
    use axum::response::sse::Event;
    async_stream::stream! {
        loop {
            if db.user_for_token(&token).ok().flatten().is_none() {
                yield Ok(Event::default().event("auth").data("session expired or revoked"));
                break;
            }
            let received = tokio::select! {
                event = rx.recv() => event,
                _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => continue,
            };
            match received {
                Ok(event) => {
                    let Some(user) = db.user_for_token(&token).ok().flatten() else {
                        yield Ok(Event::default().event("auth").data("session expired or revoked"));
                        break;
                    };
                    if db.can_access_run(&user.id, event.run_id()).unwrap_or(false) {
                        yield Ok(Event::default().data(serde_json::to_string(&event).unwrap_or_default()));
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    }
}

pub async fn members(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(project_id): Path<String>,
) -> Response {
    let user = match require_auth(&state, &headers) {
        Ok(user) => user,
        Err(response) => return response,
    };
    match state.manager.db().project_member_ids(&user.id, &project_id) {
        Ok(ids) => Json(serde_json::json!({"user_ids":ids})).into_response(),
        Err(_) => denied(StatusCode::FORBIDDEN),
    }
}

pub async fn membership(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    method: Method,
    Path((project_id, user_id)): Path<(String, String)>,
) -> Response {
    let user = match require_auth(&state, &headers) {
        Ok(user) => user,
        Err(response) => return response,
    };
    match state.manager.db().set_project_member(
        &user.id,
        &project_id,
        &user_id,
        method == Method::PUT,
    ) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => denied(StatusCode::FORBIDDEN),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Body, routing::any};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tower::ServiceExt;

    fn fixture() -> Arc<tj_db::Database> {
        let db = Arc::new(tj_db::Database::open(":memory:").unwrap());
        for id in ["admin", "alice", "bob"] {
            db.create_user(&tj_db::User {
                id: id.into(),
                username: id.into(),
                password_hash: "unused".into(),
                display_name: None,
                is_admin: id == "admin",
                created_at: String::new(),
            })
            .unwrap();
            db.issue_token(id, id).unwrap();
        }
        for id in ["a", "b"] {
            db.save_project(&tj_db::Project {
                id: id.into(),
                name: id.into(),
                repo_url: None,
                base_branch: "main".into(),
                workspace_path: None,
                git_provider: None,
                profile: "full".into(),
                workspace_kind: None,
                catalog_id: None,
                provisioning_state: None,
                provisioning_error: None,
                created_at: String::new(),
                updated_at: String::new(),
            })
            .unwrap();
            db.save_run(&tj_db::PipelineRun::new(
                format!("run-{id}"),
                Some(tj_db::ProjectId(id.into())),
                None,
                id.into(),
                tj_db::RunStatus::Running,
                None,
                None,
                None,
                None,
            ))
            .unwrap();
        }
        db.set_project_member("admin", "a", "alice", true).unwrap();
        db.set_project_member("admin", "b", "bob", true).unwrap();
        db
    }

    fn app(db: Arc<tj_db::Database>, path: &str, calls: Arc<AtomicUsize>) -> Router {
        Router::new()
            .route(
                path,
                any(move || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    async { StatusCode::NO_CONTENT }
                }),
            )
            .route_layer(axum::middleware::from_fn_with_state(db, authorize))
    }

    async fn request(app: Router, path: &str, token: &str, method: Method) -> StatusCode {
        app.oneshot(
            axum::http::Request::builder()
                .method(method)
                .uri(path)
                .header("Authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
    }

    #[tokio::test]
    async fn cross_project_reads_and_mutations_never_reach_handlers() {
        let db = fixture();
        let calls = Arc::new(AtomicUsize::new(0));
        for suffix in [
            "",
            "/tree",
            "/events",
            "/decisions",
            "/stage-logs",
            "/files",
            "/artifacts",
            "/input",
            "/questions",
            "/bundle",
            "/budget",
            "/recovery",
            "/retry",
            "/approve",
            "/cancel",
            "/merge-complete",
            "/delivery",
            "/delivery/releases",
            "/delivery/decisions",
            "/delivery/releases/{release_id}/reconcile",
            "/delivery/releases/{release_id}/retry-dispatch",
        ] {
            let route = format!("/v1/pipeline/runs/{{id}}{suffix}");
            for method in [Method::GET, Method::POST, Method::DELETE] {
                let router = app(db.clone(), &route, calls.clone());
                assert_eq!(
                    request(
                        router.clone(),
                        &format!("/v1/pipeline/runs/run-b{suffix}"),
                        "alice",
                        method.clone()
                    )
                    .await,
                    StatusCode::FORBIDDEN
                );
                assert_eq!(
                    request(
                        router,
                        &format!("/v1/pipeline/runs/run%2Db{suffix}"),
                        "alice",
                        method
                    )
                    .await,
                    StatusCode::FORBIDDEN
                );
            }
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let router = app(db, "/v1/pipeline/runs/{id}", calls.clone());
        assert_eq!(
            request(router, "/v1/pipeline/runs/run-a", "alice", Method::GET).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn project_members_cannot_administer_credentials_accounts_or_membership() {
        let db = fixture();
        let calls = Arc::new(AtomicUsize::new(0));
        for (route, path) in [
            (
                "/v1/projects/{project_id}/secrets",
                "/v1/projects/a/secrets",
            ),
            (
                "/v1/projects/{project_id}/secrets/{key}",
                "/v1/projects/a/secrets/token",
            ),
            (
                "/v1/projects/{project_id}/ssh-key",
                "/v1/projects/a/ssh-key",
            ),
            (
                "/v1/projects/{project_id}/members/{user_id}",
                "/v1/projects/a/members/alice",
            ),
            ("/v1/users", "/v1/users"),
            ("/api/logs/system", "/api/logs/system"),
            ("/v1/workflows/import", "/v1/workflows/import"),
            ("/v1/lanes/{id}/workflow", "/v1/lanes/lane/workflow"),
        ] {
            let router = app(db.clone(), route, calls.clone());
            assert_eq!(
                request(router.clone(), path, "alice", Method::POST).await,
                StatusCode::FORBIDDEN
            );
            assert_eq!(
                request(router, path, "admin", Method::POST).await,
                StatusCode::NO_CONTENT
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 8);
    }

    #[tokio::test]
    async fn membership_and_session_revocation_take_effect_on_next_request() {
        let db = fixture();
        let router = app(
            db.clone(),
            "/v1/pipeline/runs/{id}",
            Arc::new(AtomicUsize::new(0)),
        );
        db.set_project_member("admin", "a", "alice", false).unwrap();
        assert_eq!(
            request(
                router.clone(),
                "/v1/pipeline/runs/run-a",
                "alice",
                Method::GET
            )
            .await,
            StatusCode::FORBIDDEN
        );
        db.revoke_token("bob").unwrap();
        assert_eq!(
            request(router, "/v1/pipeline/runs/run-b", "bob", Method::GET).await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn streaming_endpoint_checks_project_before_handler() {
        let calls = Arc::new(AtomicUsize::new(0));
        let router = app(
            fixture(),
            "/v1/pipeline/runs/{id}/stage-logs/stream",
            calls.clone(),
        );
        assert_eq!(
            request(
                router.clone(),
                "/v1/pipeline/runs/run-b/stage-logs/stream?token=alice",
                "",
                Method::GET
            )
            .await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            request(
                router,
                "/v1/pipeline/runs/run-a/stage-logs/stream?token=alice",
                "",
                Method::GET
            )
            .await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn delivery_callbacks_are_hmac_routes_not_user_routes() {
        let calls = Arc::new(AtomicUsize::new(0));
        let router = app(
            fixture(),
            "/v1/delivery/releases/{release_id}/callbacks",
            calls.clone(),
        );
        assert_eq!(
            request(
                router,
                "/v1/delivery/releases/release-1/callbacks",
                "",
                Method::POST
            )
            .await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn open_event_stream_filters_other_projects_and_closes_after_revocation() {
        use http_body_util::BodyExt;
        use tokio_stream::StreamExt;
        let db = fixture();
        let (tx, rx) = tokio::sync::broadcast::channel(8);
        let event = |id: &str| tj_db::PipelineNotification::FlowCompleted {
            run_id: id.into(),
            flow: "flow-a".into(),
            summary: id.into(),
        };
        tx.send(event("run-b")).unwrap();
        tx.send(event("run-a")).unwrap();
        let mut stream = Box::pin(pipeline_events(db.clone(), "alice".into(), rx));
        let first = stream.next().await.unwrap().unwrap();
        let response = axum::response::Sse::new(tokio_stream::iter([Ok::<
            _,
            std::convert::Infallible,
        >(first)]))
        .into_response();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(text.contains("run-a"));
        assert!(!text.contains("run-b"));
        db.revoke_token("alice").unwrap();
        tx.send(event("run-a")).unwrap();
        let revoked = stream.next().await.unwrap().unwrap();
        let response = axum::response::Sse::new(tokio_stream::iter([Ok::<
            _,
            std::convert::Infallible,
        >(revoked)]))
        .into_response();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(text.contains("event: auth"));
        assert!(!text.contains("run-a"));
        assert!(stream.next().await.is_none());
    }
}
