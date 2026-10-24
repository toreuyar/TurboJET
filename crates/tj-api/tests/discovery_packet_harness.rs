//! TJ-019/TJ-020/TJ-021/TJ-022 integrated Discovery packet journey.
//!
//! Real Axum API → Manager → Flow → Stage → SQLite/workspace with a scripted
//! OpenAI-compatible model. No git/forge/SSH/Zulip/live provider is used.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header, request::Builder};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tj_api::handlers::build_app;
use tj_manager::{ManagerHandle, spawn_manager};
use tower::ServiceExt;

struct ScriptedModel {
    port: u16,
    responses: Arc<Mutex<VecDeque<Value>>>,
    requests: Arc<AtomicUsize>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl ScriptedModel {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let responses = Arc::new(Mutex::new(VecDeque::new()));
        let requests = Arc::new(AtomicUsize::new(0));
        let (shutdown, mut stop) = tokio::sync::oneshot::channel::<()>();
        let responses_for_server = Arc::clone(&responses);
        let requests_for_server = Arc::clone(&requests);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((mut stream, _)) = accepted else { continue; };
                        let responses = Arc::clone(&responses_for_server);
                        let requests = Arc::clone(&requests_for_server);
                        tokio::spawn(async move {
                            use tokio::io::{AsyncReadExt, AsyncWriteExt};
                            let mut buf = vec![0u8; 131072];
                            let _ = stream.read(&mut buf).await;
                            requests.fetch_add(1, Ordering::SeqCst);
                            let response = responses.lock().unwrap().pop_front()
                                .unwrap_or_else(|| text_response("no scripted response left"));
                            let body = serde_json::to_string(&response).unwrap();
                            let http = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                body.len(), body
                            );
                            let _ = stream.write_all(http.as_bytes()).await;
                            let _ = stream.flush().await;
                        });
                    }
                    _ = &mut stop => break,
                }
            }
        });
        Self {
            port,
            responses,
            requests,
            shutdown: Some(shutdown),
        }
    }
    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
    fn push(&self, response: Value) {
        self.responses.lock().unwrap().push_back(response);
    }
    fn request_count(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

impl Drop for ScriptedModel {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

fn text_response(text: &str) -> Value {
    json!({
        "id": "chatcmpl-packet", "object": "chat.completion", "model": "scripted-model",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": text}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7}
    })
}

fn packet_response(packet: Value) -> Value {
    text_response(&json!({
        "verdict": "generated", "summary": "generated packet", "content": "# Requirements packet",
        "packet": packet
    }).to_string())
}

fn fixture_packet(name: &str) -> Value {
    let raw = match name {
        "complete" => include_str!("../../tj-packet/fixtures/complete.json"),
        "contradictory" => include_str!("../../tj-packet/fixtures/contradictory.json"),
        _ => panic!("unknown fixture"),
    };
    let mut value: Value = serde_json::from_str(raw).unwrap();
    // The fixture's evidence references are illustrative. In this runtime the
    // packet stage's own frozen artifact is the durable source, so map every
    // source to that exact artifact reference.
    for source in value["sources"].as_array_mut().unwrap() {
        source["kind"] = json!("stage_artifact");
        source["reference"] = json!("artifact:discovery:packet");
    }
    value
}

struct Harness {
    _temp: TempDir,
    manager: ManagerHandle,
    app: axum::Router,
    token: String,
    project_id: String,
    lane_id: String,
    workspace_path: String,
}

impl Harness {
    async fn start(model: &ScriptedModel) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let config_dir = root.join("config");
        let package_dir = config_dir.join("package");
        let agent_dir = package_dir.join("agents/packet");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::create_dir_all(package_dir.join("schemas")).unwrap();
        std::fs::write(
            agent_dir.join("prompt.md"),
            "Generate the requirements packet from the run inputs.",
        )
        .unwrap();
        std::fs::write(
            package_dir.join("schemas/packet.json"),
            json!({
                "type": "object", "required": ["verdict", "summary", "packet"],
                "properties": {
                    "verdict": {"type": "string", "enum": ["generated"]},
                    "summary": {"type": "string"},
                    "packet": {"type": "object"}
                }
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            agent_dir.join("agent.json"),
            json!({
                "id": "packet", "name": "Packet", "prompt": "prompt.md",
                "schema": "schemas/packet.json", "timeout": 20, "max_output_retries": 3,
                "output": {"artifacts": [{"name": "output.md", "required": true, "verify": "non-empty"}]},
                "tools": {"read": true, "write": true, "edit": false, "bash": false}
            }).to_string(),
        ).unwrap();
        let workflow = json!({
            "version": 1, "name": "Discovery packet harness", "catalog": {"id": "synthetic-discovery-harness"}, "maturity": "supported",
            "capabilities": {
                "schema_version": 1,
                "workspace": {"kind": "none"},
                "gates": {"discovery-gate": "packet"},
                "requirements_packet": {
                    "produce_flow": "discovery", "producer_stage": "packet",
                    "binding_gate": "discovery-gate"
                }
            },
            "description": "Repository-free Discovery profile", "entry": "discovery", "exit": "discovery-gate",
            "limits": {"maxIterations": 2, "maxSteps": 6, "maxDepth": 3},
            "flow-order": ["discovery", "discovery-gate"],
            "flows": {
                "discovery": {"type": "flow", "entry": "packet", "exit": "packet", "stages": {
                    "packet": {"agent": "packet", "interactive": false,
                        "routes": {"generated": "_next-flow"}}
                }},
                "discovery-gate": {"type": "gate", "routes": {"approved": "_next-flow", "rejected": "_rework"}, "stages": {}}
            }
        });
        // Synthetic package imported from the configured source directory.
        std::fs::write(package_dir.join("workflow.smg.json"), workflow.to_string()).unwrap();
        let key_env = format!("TJ_PACKET_KEY_{}", uuid::Uuid::new_v4().simple());
        // Safety: unique key set before any worker reads it.
        unsafe {
            std::env::set_var(&key_env, "unused-test-key");
        }
        std::fs::write(
            config_dir.join("turbojet.json"),
            json!({
                "listen_addr": "127.0.0.1:0", "log_level": "warn",
                "database_path": root.join("state.db").to_string_lossy(),
                "workspace_base": root.join("workspaces").to_string_lossy(),
                "workflow_sources": [{"id": "discovery-harness", "path": "config/package", "kind": "package"}],
                "providers": [{"id": "scripted", "type": "open_ai", "api_key_env": key_env,
                    "base_url": model.base_url(), "default_model": "scripted-model"}],
                "agents": [{"id": "chief", "provider_id": "scripted", "model": "scripted-model",
                    "default": true, "enabled": true, "tools": [], "system_prompt": "test"}],
                "budgets": {"question_rounds_per_stage": 6, "question_rounds_per_run": 12,
                    "model_calls": 20, "tool_calls": 20, "output_corrections": 4,
                    "interaction_nudges": 4, "active_secs": 120},
                "run_timeout_secs": 60
            })
            .to_string(),
        )
        .unwrap();

        let loader = tj_config::ConfigLoader::from_file(&config_dir.join("turbojet.json")).unwrap();
        let spawn = spawn_manager(loader).await.unwrap();
        let manager = spawn.handle.clone();
        let app = build_app(spawn).await.unwrap();

        let (status, auth) = call(
            &app,
            Method::POST,
            "/v1/auth/register",
            None,
            Some(json!({"username": "admin", "password": "harness-pass"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "register: {auth}");
        let token = auth["token"].as_str().unwrap().to_string();
        let (status, project) = call(
            &app,
            Method::POST,
            "/v1/projects",
            Some(&token),
            Some(json!({"name": "Discovery", "catalog_id": "synthetic-discovery-harness"})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "project: {project}");
        let project_id = project["project"]["id"].as_str().unwrap().to_string();
        let lane_id = project["lane"]["id"].as_str().unwrap().to_string();
        let workspace_path = project["project"]["workspace_path"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(project["project"]["profile"], "synthetic-discovery-harness");
        Self {
            _temp: temp,
            manager,
            app,
            token,
            project_id,
            lane_id,
            workspace_path,
        }
    }

    async fn create_run(&self, description: &str) -> String {
        let (status, body) = call(&self.app, Method::POST, "/v1/pipeline/runs", Some(&self.token),
            Some(json!({"description": description, "project_id": self.project_id, "lane_id": self.lane_id}))).await;
        assert_eq!(status, StatusCode::OK, "create run: {body}");
        body["id"].as_str().unwrap().to_string()
    }

    async fn tree(&self, run_id: &str) -> Value {
        let (status, body) = call(
            &self.app,
            Method::GET,
            &format!("/v1/pipeline/runs/{run_id}/tree"),
            Some(&self.token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "tree: {body}");
        body
    }

    async fn wait_for<F>(&self, run_id: &str, predicate: F) -> Value
    where
        F: Fn(&Value) -> bool,
    {
        for _ in 0..200 {
            let tree = self.tree(run_id).await;
            if predicate(&tree) {
                return tree;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!(
            "run {run_id} did not reach expected state; tree={}",
            self.tree(run_id).await
        );
    }

    async fn packet(&self, run_id: &str) -> Value {
        let (status, body) = call(
            &self.app,
            Method::GET,
            &format!("/v1/pipeline/runs/{run_id}/packet"),
            Some(&self.token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "packet: {body}");
        body
    }

    async fn shutdown(self) {
        let _ = self.manager.shutdown().await;
    }
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
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn call_raw(
    app: &axum::Router,
    path: &str,
    token: Option<&str>,
) -> (StatusCode, HeaderMap, String) {
    let mut builder = Request::builder().method(Method::GET).uri(path);
    if let Some(token) = token {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, String::from_utf8_lossy(&bytes).to_string())
}

use axum::http::HeaderMap;

#[tokio::test]
async fn repository_free_discovery_generates_reviews_revises_approves_and_exports_the_exact_packet()
{
    let model = ScriptedModel::start().await;
    let harness = Harness::start(&model).await;

    // No git provisioning, worktree, forge or shell: the project workspace is
    // a plain durable artifact directory.
    assert!(
        !std::path::Path::new(&harness.workspace_path)
            .join(".git")
            .exists()
    );
    assert!(
        !std::path::Path::new(&harness.workspace_path)
            .join("runs")
            .exists()
    );

    model.push(packet_response(fixture_packet("complete")));
    let run = harness.create_run("Repository-free Discovery").await;
    let tree = harness
        .wait_for(&run, |tree| tree["run"]["status"] == "waiting_for_gate")
        .await;
    assert_eq!(
        model.request_count(),
        1,
        "only the packet stage calls the model"
    );
    assert_eq!(tree["run"]["profile"], "synthetic-discovery-harness");
    assert_eq!(
        tree["flows"].as_array().unwrap().len(),
        1,
        "no engineering flow was entered"
    );
    assert_eq!(tree["flows"][0]["flow_name"], "discovery");
    assert!(
        !std::path::Path::new(&harness.workspace_path)
            .join(".git")
            .exists()
    );
    let run_workspace = std::path::Path::new(&harness.workspace_path)
        .join("runs")
        .join(&run);
    assert!(
        run_workspace.exists(),
        "each Discovery run has an isolated artifact workspace"
    );

    let packet = harness.packet(&run).await;
    let latest = packet["latest"].clone();
    assert_eq!(latest["completeness"], "complete");
    assert_eq!(
        latest["content"]["title"],
        "Booking cancellation for solo travellers"
    );
    assert_eq!(
        latest["content"]["sources"][0]["reference"],
        "artifact:discovery:packet"
    );
    assert_eq!(latest["content_hash"].as_str().unwrap().len(), 64);
    assert_eq!(
        latest["source_artifact_id"].as_str().unwrap().is_empty(),
        false
    );
    assert_eq!(packet["gate_binding"]["revision_id"], latest["id"]);
    assert_eq!(
        packet["gate_binding"]["content_hash"],
        latest["content_hash"]
    );
    assert_eq!(packet["revisions"].as_array().unwrap().len(), 1);
    let version = tree["run"]["state_version"].as_i64().unwrap();
    let first_revision = latest["id"].as_str().unwrap().to_string();
    let first_hash = latest["content_hash"].as_str().unwrap().to_string();

    // Portable exports before approval carry the same revision/hash/provenance.
    let export_json_path =
        format!("/v1/pipeline/runs/{run}/packet/export?format=json&revision={first_revision}");
    let (status, headers, body) =
        call_raw(&harness.app, &export_json_path, Some(&harness.token)).await;
    assert_eq!(status, StatusCode::OK, "export: {body}");
    assert!(
        headers
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("application/json")
    );
    assert!(
        headers
            .get(header::CONTENT_DISPOSITION)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("pkt_")
    );
    let exported: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(exported["packet"]["content_hash"], first_hash);
    assert_eq!(exported["packet"]["revision"], 1);
    assert_eq!(exported["provenance"]["stage_name"], "packet");
    assert_eq!(exported["history"].as_array().unwrap().len(), 1);
    let (status, headers, markdown) = call_raw(
        &harness.app,
        &format!("/v1/pipeline/runs/{run}/packet/export?format=markdown&revision={first_revision}"),
        Some(&harness.token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("text/markdown")
    );
    assert!(markdown.contains(&first_hash));
    assert!(markdown.contains("Booking cancellation for solo travellers"));
    assert!(markdown.contains("artifact:discovery:packet"));
    assert!(markdown.contains("Provenance actor"));

    // A reviewer revision changes the packet and rebinds the open gate. The old
    // approval identity and state version must conflict.
    let mut changed = fixture_packet("complete");
    changed["title"] = json!("Booking cancellation — reviewer revision");
    let (status, revision) = call(&harness.app, Method::POST,
        &format!("/v1/pipeline/runs/{run}/packet/revisions"), Some(&harness.token),
        Some(json!({"content": changed, "packet_id": latest["packet_id"], "change_request": "clarify title"}))).await;
    assert_eq!(status, StatusCode::CREATED, "revision: {revision}");
    assert_eq!(revision["revision"], 2);
    assert_eq!(revision["parent_revision_id"], first_revision);
    assert_ne!(revision["content_hash"], first_hash);

    // Retrying identical content is a replay and adds no history.
    let (status, replay) = call(
        &harness.app,
        Method::POST,
        &format!("/v1/pipeline/runs/{run}/packet/revisions"),
        Some(&harness.token),
        Some(json!({"content": changed, "packet_id": latest["packet_id"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["id"], revision["id"]);

    let refreshed = harness.packet(&run).await;
    assert_eq!(refreshed["revisions"].as_array().unwrap().len(), 2);
    assert_eq!(refreshed["gate_binding"]["revision_id"], revision["id"]);
    let refreshed_tree = harness.tree(&run).await;
    let refreshed_version = refreshed_tree["run"]["state_version"].as_i64().unwrap();
    assert!(
        refreshed_version > version,
        "revision rebind bumps the run version"
    );

    let stale = json!({
        "approved": true, "gate_instance_id": refreshed_tree["run"]["gate_instance_id"],
        "expected_state_version": version, "idempotency_key": "stale-approval",
        "packet_revision_id": first_revision, "packet_content_hash": first_hash
    });
    let (status, body) = call(
        &harness.app,
        Method::POST,
        &format!("/v1/pipeline/runs/{run}/approve"),
        Some(&harness.token),
        Some(stale),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "stale review must conflict: {body}"
    );
    assert_eq!(
        harness.tree(&run).await["run"]["status"],
        "waiting_for_gate"
    );

    let approved = json!({
        "approved": true, "gate_instance_id": refreshed_tree["run"]["gate_instance_id"],
        "expected_state_version": refreshed_version, "idempotency_key": "exact-packet-approval",
        "packet_revision_id": revision["id"], "packet_content_hash": revision["content_hash"]
    });
    let (status, receipt) = call(
        &harness.app,
        Method::POST,
        &format!("/v1/pipeline/runs/{run}/approve"),
        Some(&harness.token),
        Some(approved),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "approve: {receipt}");
    let final_tree = harness
        .wait_for(&run, |tree| tree["run"]["status"] == "completed")
        .await;
    assert_eq!(
        model.request_count(),
        1,
        "approval ends Discovery without another stage/model call"
    );
    assert_eq!(
        final_tree["flows"].as_array().unwrap().len(),
        1,
        "no Engineering/Delivery flow"
    );
    assert!(
        !final_tree["flows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|flow| matches!(
                flow["flow_name"].as_str(),
                Some("planning" | "engineering" | "delivery")
            ))
    );

    let decisions = call(
        &harness.app,
        Method::GET,
        &format!("/v1/pipeline/runs/{run}/decisions"),
        Some(&harness.token),
        None,
    )
    .await
    .1;
    assert_eq!(decisions[0]["packet_revision_id"], revision["id"]);
    assert_eq!(
        decisions[0]["packet_content_hash"],
        revision["content_hash"]
    );

    // Exports after approval contain the same version/hash/provenance and the
    // approval history, not a transcript.
    let (status, _headers, body) = call_raw(
        &harness.app,
        &format!("/v1/pipeline/runs/{run}/packet/export?format=json"),
        Some(&harness.token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let exported: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(exported["packet"]["revision"], 2);
    assert_eq!(exported["packet"]["content_hash"], revision["content_hash"]);
    assert_eq!(exported["history"].as_array().unwrap().len(), 2);
    assert_eq!(exported["reviews"][0]["outcome"], "approved");
    assert_eq!(exported["reviews"][0]["actor_name"], "admin");
    let (status, _headers, markdown) = call_raw(
        &harness.app,
        &format!("/v1/pipeline/runs/{run}/packet/export?format=markdown"),
        Some(&harness.token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(markdown.contains("approved by admin"));
    assert!(markdown.contains(&revision["content_hash"].as_str().unwrap().to_string()));
    assert!(markdown.contains("clarify title"));

    // TJ-023/TJ-024: readiness, durable activity resync, diagnostics and a
    // credential-safe support export are available on the real API.
    let (status, headers, ready_body) = call_raw(&harness.app, "/api/ready", None).await;
    assert_eq!(status, StatusCode::OK, "ready: {ready_body}");
    let ready: Value = serde_json::from_str(&ready_body).unwrap();
    assert!(matches!(
        ready["status"].as_str(),
        Some("ready" | "degraded")
    ));
    assert_eq!(ready["provider"]["ready"], true);
    assert!(
        headers
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("application/json")
    );
    let (status, readiness) = call(
        &harness.app,
        Method::GET,
        "/v1/readiness",
        Some(&harness.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(readiness["first_run"]["admin_exists"], true);

    harness
        .manager
        .db()
        .append_stage_log(&tj_db::StageLog {
            id: 0,
            run_id: tj_db::RunId(run.clone()),
            stage_id: "stage_sentinel".into(),
            stage_name: Some("packet".into()),
            flow_name: Some("discovery".into()),
            kind: "tool_result".into(),
            content: "Authorization: Bearer sentinel-secret-token api_key=sk-live-nested".into(),
            created_at: chrono::Utc::now().to_rfc3339(),
        })
        .unwrap();
    let (status, activity) = call(
        &harness.app,
        Method::GET,
        &format!("/v1/pipeline/runs/{run}/activity?after=0"),
        Some(&harness.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(activity["resync"].as_bool().unwrap_or(false));
    assert!(activity["next_after"].as_i64().unwrap_or(0) > 0);
    let (status, diag) = call(
        &harness.app,
        Method::GET,
        &format!("/v1/pipeline/runs/{run}/diagnostics"),
        Some(&harness.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(diag["provider_usage"]["calls"].as_u64().unwrap_or(0) >= 1);
    assert_eq!(diag["stages"][0]["verdict"], "generated");
    let (status, headers, export) = call_raw(
        &harness.app,
        &format!("/v1/pipeline/runs/{run}/support-export"),
        Some(&harness.token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers
            .get(header::CONTENT_DISPOSITION)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("turbojet-support-")
    );
    assert!(
        !export.contains("sentinel-secret-token"),
        "support export leaked a bearer token"
    );
    assert!(
        !export.contains("sk-live-nested"),
        "support export leaked an API key"
    );
    assert!(export.contains("Credentials, bearer tokens"));

    // Terminal runs cannot be rewritten; auth/format errors are explicit.
    let (status, _) = call(
        &harness.app,
        Method::POST,
        &format!("/v1/pipeline/runs/{run}/packet/revisions"),
        Some(&harness.token),
        Some(json!({"content": changed})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _, _) = call_raw(
        &harness.app,
        &format!("/v1/pipeline/runs/{run}/packet/export?format=json"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(
        &harness.app,
        Method::GET,
        &format!("/v1/pipeline/runs/{run}/packet/export?format=exe"),
        Some(&harness.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    harness.shutdown().await;
}

#[tokio::test]
async fn contradictory_or_fabricated_packets_cannot_be_approved() {
    let model = ScriptedModel::start().await;
    let harness = Harness::start(&model).await;
    model.push(packet_response(fixture_packet("contradictory")));
    let run = harness.create_run("Contradictory Discovery").await;
    let tree = harness
        .wait_for(&run, |tree| tree["run"]["status"] == "waiting_for_gate")
        .await;
    let packet = harness.packet(&run).await;
    assert_eq!(packet["latest"]["completeness"], "contradictory");
    let version = tree["run"]["state_version"].as_i64().unwrap();
    let approve = json!({
        "approved": true, "gate_instance_id": tree["run"]["gate_instance_id"],
        "expected_state_version": version, "idempotency_key": "contradictory-approval",
        "packet_revision_id": packet["latest"]["id"], "packet_content_hash": packet["latest"]["content_hash"],
        "acknowledge_incomplete": true
    });
    let (status, body) = call(
        &harness.app,
        Method::POST,
        &format!("/v1/pipeline/runs/{run}/approve"),
        Some(&harness.token),
        Some(approve),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "contradictory approval: {body}"
    );
    assert_eq!(
        harness.tree(&run).await["run"]["status"],
        "waiting_for_gate"
    );

    // A reviewer cannot invent a source reference that no durable input backs.
    let mut fabricated = fixture_packet("complete");
    fabricated["sources"][0]["reference"] = json!("artifact:discovery:invented");
    let (status, body) = call(
        &harness.app,
        Method::POST,
        &format!("/v1/pipeline/runs/{run}/packet/revisions"),
        Some(&harness.token),
        Some(json!({"content": fabricated, "change_request": "invent evidence"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "fabricated source: {body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("cannot be fabricated")
    );
    assert_eq!(
        harness.packet(&run).await["revisions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    harness.shutdown().await;
}
