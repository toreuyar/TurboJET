//! TJ-039/TJ-040 local delivery integration drills.
//!
//! A disposable HTTP server implements the Forgejo Actions workflow-dispatch
//! and run-list subset. The real Axum API, SQLite durability and HMAC callback
//! path are exercised. No real CI, deployment target, SSH or forge is contacted;
//! this is local contract evidence only, never live delivery proof.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header, request::Builder};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tj_api::handlers::build_app;
use tj_db::publications::NewPublication;
use tj_db::{LaneId, PipelineRun, ProjectId, RunStatus};
use tj_delivery::{
    CALLBACK_EVENT_HEADER, CALLBACK_SIGNATURE_HEADER, CALLBACK_SOURCE_HEADER,
    CALLBACK_TIMESTAMP_HEADER, CallbackEvent, CallbackKind, CiState, DeploymentState,
    ObservationState, ReleaseEnvironment, VerificationState, sign_callback,
};
use tj_manager::{ManagerHandle, spawn_manager};
use tower::ServiceExt;

struct DeliveryForge {
    base_url: String,
    dispatches: Arc<Mutex<Vec<Value>>>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl DeliveryForge {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let dispatches = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&dispatches);
        let counter = Arc::new(AtomicUsize::new(0));
        let (shutdown, mut stop) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((mut stream, _)) = accepted else { continue };
                        let recorded = Arc::clone(&recorded);
                        let counter = Arc::clone(&counter);
                        tokio::spawn(async move {
                            use tokio::io::{AsyncReadExt, AsyncWriteExt};
                            let mut buf = Vec::new();
                            let mut chunk = [0u8; 8192];
                            let header_end = loop {
                                let n = stream.read(&mut chunk).await.unwrap_or(0);
                                if n == 0 { return; }
                                buf.extend_from_slice(&chunk[..n]);
                                if let Some(index) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                                    break index + 4;
                                }
                            };
                            let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
                            let request_line = headers.lines().next().unwrap_or_default().to_string();
                            let content_length = headers.lines()
                                .find_map(|line| line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|value| value.trim().parse::<usize>().ok()))
                                .unwrap_or(0);
                            let mut body = buf[header_end..].to_vec();
                            while body.len() < content_length {
                                let n = stream.read(&mut chunk).await.unwrap_or(0);
                                if n == 0 { break; }
                                body.extend_from_slice(&chunk[..n]);
                            }
                            if request_line.starts_with("POST /api/v1/repos/owner/repo/actions/workflows/delivery.yml/dispatches") {
                                let input: Value = serde_json::from_slice(&body).unwrap_or_default();
                                recorded.lock().unwrap().push(input);
                                let id = counter.fetch_add(1, Ordering::SeqCst) + 42;
                                let body = serde_json::to_vec(&json!({
                                    "id": id,
                                    "html_url": format!("http://localhost/actions/{id}"),
                                })).unwrap();
                                let head = format!("HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                                let _ = stream.write_all(head.as_bytes()).await;
                                let _ = stream.write_all(&body).await;
                            } else if request_line.starts_with("GET /api/v1/repos/owner/repo/actions/runs") {
                                let body = serde_json::to_vec(&json!({"workflow_runs":[]})).unwrap();
                                let head = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                                let _ = stream.write_all(head.as_bytes()).await;
                                let _ = stream.write_all(&body).await;
                            } else {
                                let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                            }
                        });
                    }
                    _ = &mut stop => break,
                }
            }
        });
        Self {
            base_url,
            dispatches,
            shutdown: Some(shutdown),
        }
    }
}

impl Drop for DeliveryForge {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

struct Harness {
    _temp: TempDir,
    manager: ManagerHandle,
    app: axum::Router,
    token: String,
    project_id: String,
    run_id: String,
    publication_id: String,
    commit: String,
    secret: String,
    _forge: DeliveryForge,
}

async fn call(
    app: &axum::Router,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder: Builder = Request::builder().method(method).uri(path);
    if let Some(token) = token {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    }
    let request = match body {
        Some(body) => builder
            .header("Content-Type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

async fn callback(
    app: &axum::Router,
    release_id: &str,
    secret: &str,
    event: &CallbackEvent,
) -> (StatusCode, Value) {
    let body = serde_json::to_vec(event).unwrap();
    let timestamp = chrono::Utc::now().to_rfc3339();
    let signature = sign_callback(secret.as_bytes(), &timestamp, &event.event_id, &body);
    let mut headers = HeaderMap::new();
    headers.insert(CALLBACK_TIMESTAMP_HEADER, timestamp.parse().unwrap());
    headers.insert(CALLBACK_EVENT_HEADER, event.event_id.parse().unwrap());
    headers.insert(CALLBACK_SIGNATURE_HEADER, signature.parse().unwrap());
    headers.insert(CALLBACK_SOURCE_HEADER, event.source.parse().unwrap());
    let request = Request::builder()
        .method(Method::POST)
        .uri(format!("/v1/delivery/releases/{release_id}/callbacks"))
        .body(Body::from(body))
        .unwrap();
    let mut request = request;
    *request.headers_mut() = headers;
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn harness() -> Harness {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let forge = DeliveryForge::start().await;
    let token_env = format!("TJ_DELIVERY_TOKEN_{}", uuid::Uuid::new_v4().simple());
    let secret_env = format!("TJ_DELIVERY_SECRET_{}", uuid::Uuid::new_v4().simple());
    let key_env = format!("TJ_DELIVERY_PROVIDER_{}", uuid::Uuid::new_v4().simple());
    let secret = "local-disposable-delivery-callback-secret".to_string();
    // Safety: unique env names per test process, set before the runtime loads.
    unsafe {
        std::env::set_var(&token_env, "local-forgejo-token");
        std::env::set_var(&secret_env, &secret);
        std::env::set_var(&key_env, "unused-test-key");
    }
    let config_dir = root.join("config");
    let package_dir = config_dir.join("package");
    let agent_dir = package_dir.join("agents/helper");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::create_dir_all(package_dir.join("schemas")).unwrap();
    std::fs::write(agent_dir.join("prompt.md"), "no model call in this harness").unwrap();
    std::fs::write(
        package_dir.join("schemas/helper.json"),
        json!({
            "type": "object", "required": ["verdict"],
            "properties": {"verdict": {"type": "string", "enum": ["success"]}}
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        agent_dir.join("agent.json"),
        json!({
            "id": "helper", "name": "Helper", "prompt": "prompt.md",
            "schema": "schemas/helper.json", "timeout": 5,
            "max_output_retries": 0,
            "tools": {"read": false, "write": false, "edit": false, "bash": false}
        })
        .to_string(),
    )
    .unwrap();
    let workflow = json!({
        "version": 1, "name": "delivery-harness", "catalog": {"id": "synthetic-delivery-harness"}, "maturity": "experimental",
        "capabilities": {
            "schema_version": 1,
            "workspace": {"kind": "none"},
            "external_delivery": {"after_flow": "work", "explicit_request_only": true}
        },
        "description": "TJ-039/TJ-040 local delivery harness", "entry": "work", "exit": "work",
        "limits": {"maxIterations": 1, "maxSteps": 2, "maxDepth": 1},
        "flow-order": ["work"],
        "flows": {"work": {"type": "flow", "entry": "stage", "exit": "stage", "stages": {
            "stage": {"agent": "helper", "interactive": false,
                "routes": {"success": "_exit"}}
        }}}
    });
    std::fs::write(package_dir.join("workflow.smg.json"), workflow.to_string()).unwrap();
    std::fs::write(
        config_dir.join("turbojet.json"),
        json!({
            "listen_addr": "127.0.0.1:0",
            "log_level": "warn",
            "database_path": root.join("state.db").to_string_lossy(),
            "workspace_base": root.join("workspaces").to_string_lossy(),
            "workflow_sources": [{"id": "delivery-harness", "path": "config/package", "kind": "package"}],
            "providers": [{"id": "scripted", "type": "open_ai", "api_key_env": key_env,
                "base_url": "http://127.0.0.1:1", "default_model": "scripted-model"}],
            "agents": [{"id": "chief", "provider_id": "scripted", "model": "scripted-model",
                "default": true, "enabled": true, "tools": [], "system_prompt": "test"}],
            "delivery": {
                "provider": "forgejo_actions",
                "base_url": forge.base_url,
                "token_env": token_env,
                "owner": "owner",
                "repo": "repo",
                "workflow": "delivery.yml",
                "git_ref": "main",
                "callback_secret_env": secret_env,
                "callback_window_secs": 300,
                "callback_base_url": "http://turbojet.local"
            }
        })
        .to_string(),
    )
    .unwrap();

    let run_id = "run_delivery_harness".to_string();
    let loader = tj_config::ConfigLoader::from_file(&config_dir.join("turbojet.json")).unwrap();
    let loaded = loader.load_workflow_sources().unwrap().remove(0);
    let graph = loaded.graph.clone();
    let assets = tj_config::WorkflowAssets::from_entries(
        loaded
            .assets
            .iter()
            .map(|asset| (asset.logical_path.clone(), asset.content.clone())),
    )
    .unwrap();
    let bundle = tj_config::bundle::ExecutionBundle::build_from_assets(
        &loader, &run_id, None, &graph, &assets,
    )
    .unwrap();
    let spawn = spawn_manager(loader).await.unwrap();
    let manager = spawn.handle.clone();
    let app = build_app(spawn).await.unwrap();

    let (status, auth) = call(
        &app,
        Method::POST,
        "/v1/auth/register",
        None,
        Some(json!({"username": "admin", "password": "delivery-pass"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{auth}");
    let token = auth["token"].as_str().unwrap().to_string();
    let (status, project) = call(
        &app,
        Method::POST,
        "/v1/projects",
        Some(&token),
        Some(json!({"name": "Delivery Harness", "catalog_id": "synthetic-delivery-harness"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{project}");
    let project_id = project["project"]["id"].as_str().unwrap().to_string();
    let lane_id = project["lane"]["id"].as_str().unwrap().to_string();

    let db = manager.db();
    let bundle_record = tj_db::ExecutionBundleRecord {
        id: format!("bundle_{}", uuid::Uuid::new_v4()),
        run_id: tj_db::RunId(run_id.clone()),
        workflow_id: None,
        bundle_hash: bundle.bundle_hash.clone(),
        bundle_json: serde_json::to_string(&bundle).unwrap(),
        legacy: false,
        created_at: bundle.created_at.clone(),
    };
    let bundle_record = db.save_execution_bundle(&bundle_record).unwrap();
    let mut run = PipelineRun::new(
        run_id.clone(),
        Some(ProjectId(project_id.clone())),
        Some(LaneId(lane_id)),
        "delivery harness".into(),
        RunStatus::Running,
        None,
        Some("main".into()),
        Some("trace-delivery".into()),
        None,
    );
    run.set_execution_bundle(
        Some(bundle_record.id.clone()),
        Some(bundle_record.bundle_hash.clone()),
    );
    db.save_run(&run).unwrap();
    db.save_flow(&tj_db::FlowRecord::new(
        "flow_delivery_harness".into(),
        tj_db::RunId(run_id.clone()),
        "work".into(),
        Some(1),
        None,
        None,
    ))
    .unwrap();
    db.complete_flow("flow_delivery_harness").unwrap();
    run.set_status(RunStatus::Completed);
    db.save_run(&run).unwrap();
    let commit = "a".repeat(40);
    let (publication, _) = db
        .create_publication(&NewPublication {
            id: "pub_delivery".into(),
            run_id: run_id.clone(),
            flow_name: "engineering".into(),
            attempt_id: "attempt".into(),
            operation_key: "pubop-delivery".into(),
            selection_id: "selection".into(),
            task_id: "T-1".into(),
            plan_revision_id: "plan_rev".into(),
            plan_content_hash: "b".repeat(64),
            packet_revision_id: "packet_rev".into(),
            packet_content_hash: "c".repeat(64),
            branch: "tj/task".into(),
            base_branch: "main".into(),
            workspace_path: "/tmp/unused-workspace".into(),
            base_sha: Some("base".into()),
            commit_sha: Some(commit.clone()),
            test_artifact_id: "test".into(),
            test_artifact_hash: "d".repeat(64),
            review_artifact_id: "review".into(),
            review_artifact_hash: "e".repeat(64),
            acceptance_artifact_id: "acceptance".into(),
            acceptance_artifact_hash: "f".repeat(64),
            scope_files_json: "[]".into(),
            scope_violations_json: "[]".into(),
            evidence_json: None,
            draft: true,
            created_at: "2026-10-22T00:00:00Z".into(),
        })
        .unwrap();
    db.mark_push_running(&publication.id).unwrap();
    db.complete_push(&publication.id, &commit).unwrap();
    db.mark_pr_running(&publication.id).unwrap();
    db.complete_pr(&publication.id, 12, "http://forge.example/pr/12")
        .unwrap();

    Harness {
        _temp: temp,
        manager,
        app,
        token,
        project_id,
        run_id,
        publication_id: publication.id,
        commit,
        secret,
        _forge: forge,
    }
}

fn event(event_id: &str, sequence: i64, kind: CallbackKind) -> CallbackEvent {
    CallbackEvent {
        event_id: event_id.into(),
        sequence,
        source: "forgejo-actions".into(),
        occurred_at: Some(chrono::Utc::now().to_rfc3339()),
        kind,
    }
}

#[tokio::test]
async fn local_delivery_drill_binds_exact_revision_and_blocks_unknown_or_unauthorized_release() {
    let h = harness().await;
    let app = h.app.clone();
    let token = h.token.as_str();
    let base = format!("/v1/pipeline/runs/{}", h.run_id);

    // Staging requires an explicit request; nothing deployed the publication.
    let (status, created) = call(
        &app,
        Method::POST,
        &format!("{base}/delivery/releases"),
        Some(token),
        Some(json!({"environment": "staging", "publication_id": h.publication_id})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let staging_id = created["release"]["id"].as_str().unwrap().to_string();
    assert_eq!(created["release"]["dispatch_state"], "succeeded");
    assert_eq!(created["release"]["external_run_id"], "42");

    // The disposable adapter returns no run for this commit. Polling must not
    // infer success from an empty or unrelated run list; it now conflicts
    // because the durable dispatched run id '42' is absent.
    let (status, reconcile) = call(
        &app,
        Method::POST,
        &format!("{base}/delivery/releases/{staging_id}/reconcile"),
        Some(token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{reconcile}");
    assert!(
        reconcile["error"]
            .as_str()
            .unwrap()
            .contains("No external run"),
        "{reconcile}"
    );

    let staging = |kind| {
        let commit = h.commit.clone();
        match kind {
            "ci" => CallbackKind::Ci {
                status: CiState::Succeeded,
                revision: commit,
                external_run_id: "42".into(),
                external_url: Some("http://localhost/actions/42".into()),
                workflow: Some("delivery".into()),
                detail: None,
            },
            "deployment" => CallbackKind::Deployment {
                environment: ReleaseEnvironment::Staging,
                status: DeploymentState::Succeeded,
                revision: commit,
                external_run_id: Some("deploy-1".into()),
                external_url: None,
                detail: Some("staging deploy completed".into()),
            },
            "verification" => CallbackKind::Verification {
                environment: ReleaseEnvironment::Staging,
                status: VerificationState::Passed,
                revision: commit,
                verifier: "independent-health-probe".into(),
                evidence_hash: "1".repeat(64),
                detail: None,
            },
            _ => CallbackKind::Observation {
                environment: ReleaseEnvironment::Staging,
                status: ObservationState::Healthy,
                revision: commit,
                evidence_hash: "2".repeat(64),
                detail: None,
            },
        }
    };

    let ci = event("ci-1", 1, staging("ci"));
    let (status, body) = callback(&app, &staging_id, &h.secret, &ci).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let (status, body) = callback(&app, &staging_id, &h.secret, &ci).await;
    assert_eq!(status, StatusCode::OK, "duplicate: {body}");
    assert_eq!(body["replayed"], true);

    let stale = event(
        "ci-stale",
        1,
        CallbackKind::Ci {
            status: CiState::Failed,
            revision: h.commit.clone(),
            external_run_id: "42".into(),
            external_url: None,
            workflow: None,
            detail: None,
        },
    );
    let (status, body) = callback(&app, &staging_id, &h.secret, &stale).await;
    assert_eq!(status, StatusCode::CONFLICT, "stale: {body}");
    assert!(body["error"].as_str().unwrap().contains("stale"), "{body}");

    for (index, kind) in ["deployment", "verification", "observation"]
        .into_iter()
        .enumerate()
    {
        let ev = event(&format!("{kind}-1"), index as i64 + 2, staging(kind));
        let (status, body) = callback(&app, &staging_id, &h.secret, &ev).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{kind}: {body}");
    }

    let (status, bundle) = call(
        &app,
        Method::GET,
        &format!("{base}/delivery"),
        Some(token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{bundle}");
    let release = bundle["releases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|release| release["id"] == staging_id)
        .unwrap();
    assert_eq!(release["recommendation"], "ready");
    assert!(
        bundle["callbacks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["accepted"] == false)
    );

    // Production without an immutable exact-revision decision is refused.
    let (status, refused) = call(
        &app,
        Method::POST,
        &format!("{base}/delivery/releases"),
        Some(token),
        Some(json!({"environment": "production", "publication_id": h.publication_id})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .contains("authorized decision")
    );

    let (status, decision) = call(&app, Method::POST, &format!("{base}/delivery/decisions"), Some(token),
        Some(json!({
            "publication_id": h.publication_id,
            "kind": "deploy",
            "staging_release_id": staging_id,
            "idempotency_key": "decision-production-1",
            "reason": "staging CI, independent verification and observation all passed for the exact commit"
        }))).await;
    assert_eq!(status, StatusCode::CREATED, "{decision}");
    let decision_id = decision["decision"]["id"].as_str().unwrap().to_string();

    let (status, production) = call(
        &app,
        Method::POST,
        &format!("{base}/delivery/releases"),
        Some(token),
        Some(
            json!({"environment": "production", "publication_id": h.publication_id,
            "authorization_id": decision_id}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{production}");
    let production_id = production["release"]["id"].as_str().unwrap().to_string();
    let production_events = |kind, seq| match kind {
        "ci" => event(
            &format!("prod-ci-{seq}"),
            seq,
            CallbackKind::Ci {
                status: CiState::Succeeded,
                revision: h.commit.clone(),
                external_run_id: "43".into(),
                external_url: None,
                workflow: None,
                detail: None,
            },
        ),
        "deployment" => event(
            &format!("prod-deploy-{seq}"),
            seq,
            CallbackKind::Deployment {
                environment: ReleaseEnvironment::Production,
                status: DeploymentState::Succeeded,
                revision: h.commit.clone(),
                external_run_id: Some("deploy-prod".into()),
                external_url: None,
                detail: None,
            },
        ),
        "verification" => event(
            &format!("prod-verify-{seq}"),
            seq,
            CallbackKind::Verification {
                environment: ReleaseEnvironment::Production,
                status: VerificationState::Passed,
                revision: h.commit.clone(),
                verifier: "independent-prod-probe".into(),
                evidence_hash: "3".repeat(64),
                detail: None,
            },
        ),
        _ => event(
            &format!("prod-observe-{seq}"),
            seq,
            CallbackKind::Observation {
                environment: ReleaseEnvironment::Production,
                status: ObservationState::Healthy,
                revision: h.commit.clone(),
                evidence_hash: "4".repeat(64),
                detail: None,
            },
        ),
    };
    for (index, kind) in ["ci", "deployment", "verification", "observation"]
        .into_iter()
        .enumerate()
    {
        let ev = production_events(kind, index as i64 + 1);
        let (status, body) = callback(&app, &production_id, &h.secret, &ev).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{kind}: {body}");
    }

    let (_, bundle) = call(
        &app,
        Method::GET,
        &format!("{base}/delivery"),
        Some(token),
        None,
    )
    .await;
    let production_row = bundle["releases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|release| release["id"] == production_id)
        .unwrap();
    assert_eq!(production_row["recommendation"], "ready");

    // A failed production deployment blocks release and still permits an
    // explicit rollback from that failed source to a different known-good SHA.
    let failed = event(
        "prod-deploy-failed",
        5,
        CallbackKind::Deployment {
            environment: ReleaseEnvironment::Production,
            status: DeploymentState::Failed,
            revision: h.commit.clone(),
            external_run_id: Some("deploy-prod".into()),
            external_url: None,
            detail: Some("health check failed after deploy".into()),
        },
    );
    let (status, body) = callback(&app, &production_id, &h.secret, &failed).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let (_, bundle) = call(
        &app,
        Method::GET,
        &format!("{base}/delivery"),
        Some(token),
        None,
    )
    .await;
    let production_row = bundle["releases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|release| release["id"] == production_id)
        .unwrap();
    assert_eq!(production_row["recommendation"], "blocked");

    // Rollback authorization binds a different exact target revision.
    let target = "9".repeat(40);
    let (status, rollback_decision) = call(
        &app,
        Method::POST,
        &format!("{base}/delivery/decisions"),
        Some(token),
        Some(json!({
            "publication_id": h.publication_id,
            "kind": "rollback",
            "source_release_id": production_id,
            "rollback_target_sha": target,
            "idempotency_key": "decision-rollback-1",
            "reason": "deterministic local drill"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{rollback_decision}");
    let rollback_decision_id = rollback_decision["decision"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, rollback) = call(
        &app,
        Method::POST,
        &format!("{base}/delivery/releases"),
        Some(token),
        Some(
            json!({"environment": "production", "publication_id": h.publication_id,
            "kind": "rollback", "authorization_id": rollback_decision_id,
            "source_release_id": production_id, "rollback_target_sha": target}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{rollback}");
    let rollback_id = rollback["release"]["id"].as_str().unwrap().to_string();
    for (index, kind) in ["deployment", "verification", "observation"]
        .into_iter()
        .enumerate()
    {
        let seq = index as i64 + 1;
        let ev = match kind {
            "deployment" => event(
                &format!("rb-deploy-{seq}"),
                seq,
                CallbackKind::Deployment {
                    environment: ReleaseEnvironment::Production,
                    status: DeploymentState::Succeeded,
                    revision: target.clone(),
                    external_run_id: Some("rollback-1".into()),
                    external_url: None,
                    detail: None,
                },
            ),
            "verification" => event(
                &format!("rb-verify-{seq}"),
                seq,
                CallbackKind::Verification {
                    environment: ReleaseEnvironment::Production,
                    status: VerificationState::Passed,
                    revision: target.clone(),
                    verifier: "independent-rollback-probe".into(),
                    evidence_hash: "5".repeat(64),
                    detail: None,
                },
            ),
            _ => event(
                &format!("rb-observe-{seq}"),
                seq,
                CallbackKind::Observation {
                    environment: ReleaseEnvironment::Production,
                    status: ObservationState::Healthy,
                    revision: target.clone(),
                    evidence_hash: "6".repeat(64),
                    detail: None,
                },
            ),
        };
        let (status, body) = callback(&app, &rollback_id, &h.secret, &ev).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{kind}: {body}");
    }
    let (_, bundle) = call(
        &app,
        Method::GET,
        &format!("{base}/delivery"),
        Some(token),
        None,
    )
    .await;
    let rollback_row = bundle["releases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|release| release["id"] == rollback_id)
        .unwrap();
    assert_eq!(rollback_row["recommendation"], "ready");
    assert_eq!(rollback_row["expected_revision"], target);

    // A later unknown deployment is durable but never success.
    let unknown = event(
        "rb-deploy-unknown",
        4,
        CallbackKind::Deployment {
            environment: ReleaseEnvironment::Production,
            status: DeploymentState::Unknown,
            revision: target.clone(),
            external_run_id: None,
            external_url: None,
            detail: Some("health probe lost after restart".into()),
        },
    );
    let (status, body) = callback(&app, &rollback_id, &h.secret, &unknown).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let (_, bundle) = call(
        &app,
        Method::GET,
        &format!("{base}/delivery"),
        Some(token),
        None,
    )
    .await;
    let rollback_row = bundle["releases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|release| release["id"] == rollback_id)
        .unwrap();
    assert_eq!(rollback_row["deployment_state"], "unknown");
    assert_ne!(rollback_row["recommendation"], "ready");
    assert_ne!(rollback_row["recommendation"], "succeeded");

    // Unsigned callback is rejected before any state change.
    let unsigned = Request::builder()
        .method(Method::POST)
        .uri(format!("/v1/delivery/releases/{rollback_id}/callbacks"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"event_id":"unsigned"}).to_string()))
        .unwrap();
    let response = app.clone().oneshot(unsigned).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    assert_eq!(h._forge.dispatches.lock().unwrap().len(), 3);
    assert!(h._forge.dispatches.lock().unwrap().iter().any(|body| {
        body.pointer("/inputs/turbojet_environment")
            .and_then(Value::as_str)
            == Some("production")
    }));
    let _ = h.manager.shutdown().await;
    let _ = h.project_id;
}
