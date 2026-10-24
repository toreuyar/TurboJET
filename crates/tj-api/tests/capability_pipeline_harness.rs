//! Production-path proof that capability modules are driven by declarations,
//! not by flow, stage, gate, catalog or file names.
//!
//! These tests are part of the default asset-free suite: every agent, schema,
//! graph and input is created in a disposable temporary tree. A scripted local
//! OpenAI-compatible model drives the real API → Manager → Flow → Stage →
//! SQLite path. No repository, forge, live provider or shipped workflow asset
//! is used.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, request::Builder};
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
                                .unwrap_or_else(|| text_response(json!({
                                    "verdict": "success", "summary": "fallback"
                                })));
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

    fn push(&self, value: Value) {
        self.responses
            .lock()
            .unwrap()
            .push_back(text_response(value));
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

fn text_response(value: Value) -> Value {
    json!({
        "id": "chatcmpl-capability", "object": "chat.completion", "model": "scripted-model",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": value.to_string()}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7}
    })
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

struct Harness {
    _temp: TempDir,
    manager: ManagerHandle,
    app: axum::Router,
    token: String,
    project_id: String,
    lane_id: String,
    workspace: std::path::PathBuf,
}

fn write_agent(package_dir: &Path, dir: &str, verdicts: &[&str], extra: Value) {
    let agent_dir = package_dir.join("agents").join(dir);
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::create_dir_all(package_dir.join("schemas")).unwrap();
    std::fs::write(
        agent_dir.join("prompt.md"),
        format!(
            "Return one of: {}. This is a synthetic test agent.",
            verdicts.join(", ")
        ),
    )
    .unwrap();
    let mut properties = serde_json::Map::new();
    properties.insert(
        "verdict".into(),
        json!({"type": "string", "enum": verdicts}),
    );
    properties.insert("summary".into(), json!({"type": "string"}));
    if let Some(extra) = extra.as_object() {
        for (key, value) in extra {
            properties.insert(key.clone(), value.clone());
        }
    }
    std::fs::write(
        package_dir.join("schemas").join(format!("{dir}.json")),
        json!({
            "type": "object",
            "required": ["verdict", "summary"],
            "properties": properties
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        agent_dir.join("agent.json"),
        json!({
            "id": dir, "name": dir, "prompt": "prompt.md",
            "schema": format!("schemas/{dir}.json"),
            "timeout": 20, "max_output_retries": 3,
            "tools": {"read": false, "write": false, "edit": false, "bash": false}
        })
        .to_string(),
    )
    .unwrap();
}

fn graph(_package_dir: &Path, catalog: &str, capabilities: Value) -> Value {
    let agent = |name: &str| name.to_string();
    json!({
        "version": 1,
        "name": format!("{catalog} graph"),
        "catalog": {"id": catalog},
        "maturity": "experimental",
        "capabilities": capabilities,
        "description": "neutral capability production path",
        "entry": "collect",
        "exit": "deliver-code",
        "limits": {"maxIterations": 3, "maxSteps": 20, "maxDepth": 5},
        "flow-order": ["collect", "approve", "shape", "choose", "deliver-code"],
        "flows": {
            "collect": {"type": "flow", "entry": "evidence", "exit": "evidence", "stages": {
                "evidence": {"agent": agent("packet"), "interactive": false,
                    "routes": {"generated": "_next-flow"}}
            }},
            "approve": {"type": "gate", "routes": {"approved": "_next-flow", "rejected": "_rework"}, "stages": {}},
            "shape": {"type": "flow", "entry": "shape-stage", "exit": "shape-stage", "stages": {
                "shape-stage": {"agent": agent("plan"), "interactive": false,
                    "routes": {"success": "_next-flow"}}
            }},
            "choose": {"type": "gate", "routes": {"approved": "_next-flow", "rejected": "_rework"}, "stages": {}},
            "deliver-code": {"type": "flow", "entry": "draft", "exit": "draft", "stages": {
                "draft": {"agent": agent("draft"), "interactive": false,
                    "routes": {"success": "_exit"}}
            }}
        }
    })
}

fn engineering_only_graph(_package_dir: &Path) -> Value {
    let act = "act".to_string();
    json!({
        "version": 1,
        "name": "engineering without publication",
        "catalog": {"id": "engineering-without-publication"},
        "maturity": "experimental",
        "description": "a flow named engineering with no declared publication",
        "entry": "engineering",
        "exit": "engineering",
        "limits": {"maxIterations": 1, "maxSteps": 3, "maxDepth": 2},
        "flow-order": ["engineering"],
        "flows": {
            "engineering": {"type": "flow", "entry": "act", "exit": "act", "stages": {
                "act": {"agent": act, "interactive": false,
                    "routes": {"success": "_exit"}}
            }}
        }
    })
}

impl Harness {
    async fn start(model: &ScriptedModel, workflow: Value, catalog: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let config_dir = root.join("config");
        let package_dir = config_dir.join("package");
        let packet_capabilities = json!({
            "schema_version": 1,
            "workspace": {"kind": "none"},
            "inputs": {"copy_project_inputs_to": "supplied"},
            "gates": {"approve": "packet", "choose": "plan"},
            "requirements_packet": {
                "produce_flow": "collect", "producer_stage": "evidence",
                "binding_gate": "approve"
            },
            "task_plan": {
                "produce_flow": "shape", "validate_stage": "shape-stage",
                "binding_gate": "choose", "source_packet": true,
                "task_selection": {"after_gate": "choose", "required": true}
            },
            "verification": [{
                "kind": "local_citations", "flow": "deliver-code",
                "producer_stage": "draft", "inputs_dir": "supplied"
            }]
        });
        let workflow = match catalog {
            "neutral-pipeline" => graph(&package_dir, catalog, packet_capabilities),
            "engineering-without-publication" => engineering_only_graph(&package_dir),
            _ => workflow,
        };
        write_agent(
            &package_dir,
            "packet",
            &["generated"],
            json!({"packet": {"type": "object"}}),
        );
        write_agent(
            &package_dir,
            "plan",
            &["success"],
            json!({"plan": {"type": "object"}}),
        );
        write_agent(
            &package_dir,
            "draft",
            &["success"],
            json!({"citations": {"type": "array"}, "content": {"type": "string"}}),
        );
        write_agent(&package_dir, "act", &["success"], json!({}));

        std::fs::write(package_dir.join("workflow.smg.json"), workflow.to_string()).unwrap();
        let key_env = format!("TJ_CAP_KEY_{}", uuid::Uuid::new_v4().simple());
        unsafe {
            std::env::set_var(&key_env, "unused-test-key");
        }
        std::fs::write(
            config_dir.join("turbojet.json"),
            json!({
                "listen_addr": "127.0.0.1:0", "log_level": "warn",
                "database_path": root.join("state.db").to_string_lossy(),
                "workspace_base": root.join("workspaces").to_string_lossy(),
                "workflow_sources": [{
                    "id": "neutral",
                    "path": "config/package",
                    "kind": "package"
                }],
                "providers": [{"id": "scripted", "type": "open_ai", "api_key_env": key_env,
                    "base_url": model.base_url(), "default_model": "scripted-model"}],
                "agents": [{"id": "chief", "provider_id": "scripted", "model": "scripted-model",
                    "default": true, "enabled": true, "tools": [], "system_prompt": "test"}],
                "budgets": {"question_rounds_per_stage": 6, "question_rounds_per_run": 12,
                    "model_calls": 40, "tool_calls": 20, "output_corrections": 8,
                    "interaction_nudges": 6, "active_secs": 180},
                "run_timeout_secs": 90
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
        assert_eq!(status, StatusCode::OK, "{auth}");
        let token = auth["token"].as_str().unwrap().to_string();
        let (status, project) = call(
            &app,
            Method::POST,
            "/v1/projects",
            Some(&token),
            Some(json!({"name": "Neutral project", "catalog_id": catalog})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "project: {project}");
        let project_id = project["project"]["id"].as_str().unwrap().to_string();
        let lane_id = project["lane"]["id"].as_str().unwrap().to_string();
        let workspace =
            std::path::PathBuf::from(project["project"]["workspace_path"].as_str().unwrap());
        std::fs::create_dir_all(workspace.join("inputs")).unwrap();
        std::fs::write(
            workspace.join("inputs/notes.md"),
            "Monthly cost starts at 400 EUR",
        )
        .unwrap();
        Self {
            _temp: temp,
            manager,
            app,
            token,
            project_id,
            lane_id,
            workspace,
        }
    }

    async fn create_run(&self, description: &str) -> String {
        let (status, body) = call(
            &self.app,
            Method::POST,
            "/v1/pipeline/runs",
            Some(&self.token),
            Some(json!({
                "description": description,
                "project_id": self.project_id,
                "lane_id": self.lane_id
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "run: {body}");
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
        for _ in 0..400 {
            let tree = self.tree(run_id).await;
            if predicate(&tree) {
                return tree;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!(
            "run did not reach expected state: {}",
            self.tree(run_id).await
        );
    }

    async fn approve(&self, run_id: &str, body: Value) -> (StatusCode, Value) {
        call(
            &self.app,
            Method::POST,
            &format!("/v1/pipeline/runs/{run_id}/approve"),
            Some(&self.token),
            Some(body),
        )
        .await
    }
}

fn packet_response() -> Value {
    let mut packet: Value =
        serde_json::from_str(include_str!("../../tj-packet/fixtures/complete.json")).unwrap();
    for source in packet["sources"].as_array_mut().unwrap() {
        source["kind"] = json!("stage_artifact");
        source["reference"] = json!("artifact:collect:evidence");
    }
    json!({"verdict": "generated", "summary": "packet", "content": "# Packet", "packet": packet})
}

fn plan_response() -> Value {
    let plan: Value =
        serde_json::from_str(include_str!("../../tj-plan/fixtures/complete.json")).unwrap();
    json!({"verdict": "success", "summary": "plan", "content": "# Plan", "plan": plan})
}

#[tokio::test]
async fn renamed_flows_packet_plan_selection_inputs_and_citations_use_declarations() {
    let model = ScriptedModel::start().await;
    model.push(packet_response());
    model.push(plan_response());
    model.push(json!({
        "verdict": "success",
        "summary": "drafted with cited input",
        "content": "# Answer",
        "citations": [{
            "path": "supplied/notes.md",
            "excerpt": "Monthly cost starts at 400 EUR"
        }]
    }));
    let harness = Harness::start(&model, Value::Null, "neutral-pipeline").await;

    let run = harness.create_run("neutral capability pipeline").await;
    let tree = harness
        .wait_for(&run, |tree| {
            tree["run"]["status"] == "waiting_for_gate"
                && tree["run"]["gate_binding"]["kind"] == "packet"
        })
        .await;
    let (status, packet) = call(
        &harness.app,
        Method::GET,
        &format!("/v1/pipeline/runs/{run}/packet"),
        Some(&harness.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{packet}");
    let latest = packet["latest"].clone();
    let (status, receipt) = harness
        .approve(
            &run,
            json!({
                "approved": true,
                "gate_instance_id": tree["run"]["gate_instance_id"],
                "expected_state_version": tree["run"]["state_version"],
                "idempotency_key": "approve-packet",
                "packet_revision_id": latest["id"],
                "packet_content_hash": latest["content_hash"]
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{receipt}");

    let tree = harness
        .wait_for(&run, |tree| {
            tree["run"]["status"] == "waiting_for_gate"
                && tree["run"]["gate_binding"]["kind"] == "plan"
        })
        .await;
    let (status, plan) = call(
        &harness.app,
        Method::GET,
        &format!("/v1/pipeline/runs/{run}/plan"),
        Some(&harness.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    let revision = plan["latest"].clone();
    let (status, receipt) = harness
        .approve(
            &run,
            json!({
                "approved": true,
                "gate_instance_id": tree["run"]["gate_instance_id"],
                "expected_state_version": tree["run"]["state_version"],
                "idempotency_key": "approve-plan",
                "packet_revision_id": latest["id"],
                "packet_content_hash": latest["content_hash"],
                "plan_revision_id": revision["id"],
                "plan_content_hash": revision["content_hash"]
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{receipt}");

    let _tree = harness
        .wait_for(&run, |tree| {
            tree["run"]["gate_state"] == "task_selection_required"
        })
        .await;
    let (status, selection) = call(
        &harness.app,
        Method::POST,
        &format!("/v1/pipeline/runs/{run}/plan/select-task"),
        Some(&harness.token),
        Some(json!({
            "plan_revision_id": revision["id"],
            "plan_content_hash": revision["content_hash"],
            "task_id": "T-1"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "selection: {selection}");
    assert_eq!(selection["task_selection_recorded"], true);
    assert!(selection.get("engineering_release_requested").is_none());

    let tree = harness
        .wait_for(&run, |tree| tree["run"]["status"] == "completed")
        .await;
    let run_workspace = harness.workspace.join("runs").join(&run);
    assert!(
        run_workspace.join("supplied/notes.md").is_file(),
        "declared inputs were copied to the declared destination: {}",
        run_workspace.display()
    );
    assert!(
        !harness.workspace.join(".git").exists(),
        "no repository workspace was created or inferred"
    );
    assert!(
        !tree["flows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|flow| flow["flow_name"] == "publication" || flow["flow_name"] == "engineering"),
        "renamed pipeline used only declared flows: {tree}"
    );
    assert!(
        tree["flows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|flow| flow["flow_name"] == "deliver-code"),
        "declared rename is the executed flow: {tree}"
    );
    assert_eq!(model.request_count(), 3);
    let _ = harness.manager.shutdown().await;
}

#[tokio::test]
async fn flow_named_engineering_without_publication_has_no_remote_or_selection_effect() {
    let model = ScriptedModel::start().await;
    model.push(json!({"verdict": "success", "summary": "acted"}));
    let harness = Harness::start(&model, Value::Null, "engineering-without-publication").await;

    let run = harness.create_run("name inference proof").await;
    let tree = harness
        .wait_for(&run, |tree| tree["run"]["status"] == "completed")
        .await;
    assert_eq!(tree["flows"][0]["flow_name"], "engineering");
    assert!(tree["run"]["gate_state"].is_null());
    assert!(tree["run"]["gate_binding"].is_null());
    assert!(!harness.workspace.join(".git").exists());
    assert!(harness.manager.publications(&run).unwrap().is_empty());
    assert_eq!(model.request_count(), 1);
    let _ = harness.manager.shutdown().await;
}
