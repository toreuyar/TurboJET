//! TJ-030/TJ-031/TJ-032 integrated Planning journey.
//!
//! The real Axum API → Manager → Flow → Stage → SQLite/workspace chain runs a
//! scripted OpenAI-compatible local model. The graph is the same Planning shape
//! as the operator-local full-SDLC package, ending at the planning gate for
//! this pilot. No live provider, repository, forge, SSH, Zulip or shell effect
//! is used.

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
use tj_test_support::{
    SyntheticPackage, neutral_capability_package_agents, neutral_capability_workflow,
};
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
                            let mut buf = vec![0u8; 262144];
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
        "id": "chatcmpl-planning", "object": "chat.completion", "model": "scripted-model",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": text}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7}
    })
}

fn packet_response(source_reference: &str) -> Value {
    let mut packet: Value =
        serde_json::from_str(include_str!("../../tj-packet/fixtures/complete.json")).unwrap();
    for source in packet["sources"].as_array_mut().unwrap() {
        source["kind"] = json!("stage_artifact");
        source["reference"] = json!(source_reference);
    }
    text_response(&json!({
        "verdict": "generated", "summary": "generated packet", "content": "# Requirements packet",
        "packet": packet
    }).to_string())
}

fn planning_response(stage: &str) -> Value {
    match stage {
        "survey" | "outline" | "sketch" | "align" => text_response(&json!({
            "verdict": "success", "summary": format!("{stage} complete"),
            "content": format!("# {stage}\n\nDerived from the approved packet.")
        }).to_string()),
        "plan" => {
            let plan: Value = serde_json::from_str(include_str!("../../tj-plan/fixtures/complete.json")).unwrap();
            text_response(&json!({
                "verdict": "success", "summary": "traceable task plan", "content": "# Plan",
                "plan": plan
            }).to_string())
        }
        "check" => text_response(&json!({
            "verdict": "approved", "summary": "plan coverage complete",
            "content": "# Validation\n\nEvery task traces to the approved packet or an enabling rationale."
        }).to_string()),
        _ => panic!("unknown planning stage {stage}"),
    }
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
        let workflow = neutral_capability_workflow("synthetic-planning-harness", false);
        SyntheticPackage::at(
            &package_dir,
            &workflow,
            &neutral_capability_package_agents(false),
        );
        let key_env = format!("TJ_PLANNING_KEY_{}", uuid::Uuid::new_v4().simple());
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
                "workflow_sources": [{"id": "planning-harness", "path": "config/package", "kind": "package"}],
                "providers": [{"id": "scripted", "type": "open_ai", "api_key_env": key_env,
                    "base_url": model.base_url(), "default_model": "scripted-model"}],
                "agents": [{"id": "chief", "provider_id": "scripted", "model": "scripted-model",
                    "default": true, "enabled": true, "tools": [], "system_prompt": "test"}],
                "budgets": {"question_rounds_per_stage": 6, "question_rounds_per_run": 12,
                    "model_calls": 80, "tool_calls": 40, "output_corrections": 8,
                    "interaction_nudges": 6, "active_secs": 300},
                "run_timeout_secs": 120
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
            Some(json!({"name": "Planning", "catalog_id": "synthetic-planning-harness"})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "project: {project}");
        let project_id = project["project"]["id"].as_str().unwrap().to_string();
        let lane_id = project["lane"]["id"].as_str().unwrap().to_string();
        let workspace_path = project["project"]["workspace_path"]
            .as_str()
            .unwrap()
            .to_string();
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
        for _ in 0..400 {
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

    async fn plan(&self, run_id: &str) -> Value {
        let (status, body) = call(
            &self.app,
            Method::GET,
            &format!("/v1/pipeline/runs/{run_id}/plan"),
            Some(&self.token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "plan: {body}");
        body
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

    async fn shutdown(self) {
        let _ = self.manager.shutdown().await;
    }
}

fn git_head_count(workspace: &str) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-list", "--count", "HEAD"])
        .current_dir(workspace)
        .output()
        .unwrap();
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
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
) -> (StatusCode, axum::http::HeaderMap, String) {
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

fn approve_packet(tree: &Value, packet: &Value) -> Value {
    json!({
        "approved": true, "gate_instance_id": tree["run"]["gate_instance_id"],
        "expected_state_version": tree["run"]["state_version"], "idempotency_key": "approve-packet",
        "packet_revision_id": packet["latest"]["id"], "packet_content_hash": packet["latest"]["content_hash"]
    })
}

#[tokio::test]
async fn approved_packet_bounded_rework_stale_plan_and_ready_task_selection_use_the_real_pipeline()
{
    let model = ScriptedModel::start().await;
    let harness = Harness::start(&model).await;
    model.push(packet_response("artifact:collect:collector"));
    model.push(packet_response("artifact:collect:collector"));
    let run = harness.create_run("Planning pilot").await;

    let tree = harness
        .wait_for(&run, |tree| {
            tree["run"]["status"] == "waiting_for_gate"
                && tree["run"]["gate_name"] == "collect-gate"
        })
        .await;
    let packet = harness.packet(&run).await;
    assert_eq!(packet["latest"]["completeness"], "complete");
    let approved_packet_id = packet["latest"]["id"].as_str().unwrap().to_string();
    let approved_packet_hash = packet["latest"]["content_hash"]
        .as_str()
        .unwrap()
        .to_string();
    let commits_before_planning = git_head_count(&harness.workspace_path);

    for stage in ["survey", "outline", "sketch", "align", "plan", "check"] {
        model.push(planning_response(stage));
    }
    // Exact packet approval starts Planning. The packet is immutable now.
    let (status, receipt) = harness.approve(&run, approve_packet(&tree, &packet)).await;
    assert_eq!(status, StatusCode::OK, "packet approval: {receipt}");
    assert_eq!(receipt["packet_revision_id"], approved_packet_id);

    // Planning is analyze → architect → design → align → plan → validate →
    // deterministic validation → human gate.
    let (status, blocked_packet) = call(
        &harness.app,
        Method::POST,
        &format!("/v1/pipeline/runs/{run}/packet/revisions"),
        Some(&harness.token),
        Some(
            json!({"content": packet["latest"]["content"], "change_request": "should be blocked"}),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "approved packet must not be edited during Planning: {blocked_packet}"
    );
    let tree = harness
        .wait_for(&run, |tree| {
            tree["run"]["status"] == "waiting_for_gate" && tree["run"]["gate_name"] == "shape-gate"
        })
        .await;
    assert_eq!(
        model.request_count(),
        8,
        "two discovery packet stages + six planning stages"
    );
    let flows = tree["flows"].as_array().unwrap();
    let planning_runs = flows
        .iter()
        .filter(|flow| flow["flow_name"] == "shape")
        .count();
    assert_eq!(planning_runs, 1);
    let stage_names: Vec<&str> = flows
        .iter()
        .flat_map(|flow| flow["stages"].as_array().unwrap())
        .map(|stage| stage["stage_name"].as_str().unwrap())
        .collect();
    assert_eq!(
        stage_names,
        vec![
            "writer",
            "collector",
            "survey",
            "outline",
            "sketch",
            "align",
            "plan",
            "check"
        ]
    );
    assert!(
        !stage_names
            .iter()
            .any(|name| matches!(*name, "implement" | "accept"))
    );
    assert_eq!(
        tree["run"]["pr_url"],
        Value::Null,
        "Planning never opens a PR"
    );
    assert_eq!(
        git_head_count(&harness.workspace_path),
        commits_before_planning,
        "Planning must not commit repository changes"
    );

    let plan = harness.plan(&run).await;
    let latest = plan["latest"].clone();
    assert_eq!(latest["packet_revision_id"], approved_packet_id);
    assert_eq!(latest["packet_content_hash"], approved_packet_hash);
    assert_eq!(
        latest["content"]["source_packet"]["packet_id"],
        packet["latest"]["packet_id"]
    );
    assert_eq!(latest["validation"]["valid"], true);
    assert_eq!(latest["validation"]["task_count"], 4);
    assert_eq!(plan["gate_binding"]["revision_id"], latest["id"]);
    assert_eq!(
        plan["gate_binding"]["packet_revision_id"],
        approved_packet_id
    );

    // Rejected evidence: one bounded gate rejection re-enters Planning, which
    // reruns the required stages and reaches a new planning gate.
    for stage in ["survey", "outline", "sketch", "align", "plan", "check"] {
        model.push(planning_response(stage));
    }
    let reject = json!({
        "approved": false, "reason": "Plan misses measurable refund-preview verification",
        "gate_instance_id": tree["run"]["gate_instance_id"],
        "expected_state_version": tree["run"]["state_version"],
        "idempotency_key": "reject-plan-once",
        "packet_revision_id": approved_packet_id, "packet_content_hash": approved_packet_hash,
        "plan_revision_id": latest["id"], "plan_content_hash": latest["content_hash"]
    });
    let (status, receipt) = harness.approve(&run, reject).await;
    assert_eq!(status, StatusCode::OK, "plan rejection: {receipt}");
    assert_eq!(receipt["disposition"], "rework");
    let tree = harness
        .wait_for(&run, |tree| {
            tree["run"]["status"] == "waiting_for_gate" && tree["run"]["gate_name"] == "shape-gate"
        })
        .await;
    assert_eq!(
        model.request_count(),
        14,
        "one bounded Planning rework runs six stages"
    );
    let planning_runs = tree["flows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|flow| flow["flow_name"] == "shape")
        .count();
    assert_eq!(planning_runs, 2);

    let plan = harness.plan(&run).await;
    assert!(
        plan["reviews"]
            .as_array()
            .unwrap()
            .iter()
            .any(|review| review["outcome"] == "changes_requested"),
        "rejected plan review remains in history: {plan}"
    );
    let first_id = plan["latest"]["id"].as_str().unwrap().to_string();
    let first_hash = plan["latest"]["content_hash"].as_str().unwrap().to_string();
    let version = tree["run"]["state_version"].as_i64().unwrap();

    // Reviewer edit creates an immutable revision and makes the earlier plan
    // approval stale; the gate re-binds to the new plan and bumps state.
    let mut changed = plan["latest"]["content"].clone();
    changed["title"] = json!("Planning pilot — addressed review evidence");
    changed["summary"] = json!(
        "Refund-preview verification is explicit; the task plan remains traceable to the approved packet."
    );
    let (status, revision) = call(&harness.app, Method::POST,
        &format!("/v1/pipeline/runs/{run}/plan/revisions"), Some(&harness.token),
        Some(json!({"content": changed, "plan_id": plan["latest"]["plan_id"], "change_request": "address measurable evidence"}))).await;
    assert_eq!(status, StatusCode::CREATED, "plan revision: {revision}");
    assert_eq!(revision["revision"], 2);
    assert_eq!(revision["parent_revision_id"], first_id);
    assert_eq!(revision["packet_revision_id"], approved_packet_id);
    let refreshed = harness.tree(&run).await;
    assert!(refreshed["run"]["state_version"].as_i64().unwrap() > version);

    let stale = json!({
        "approved": true, "gate_instance_id": refreshed["run"]["gate_instance_id"],
        "expected_state_version": refreshed["run"]["state_version"],
        "idempotency_key": "stale-plan-approval",
        "packet_revision_id": approved_packet_id, "packet_content_hash": approved_packet_hash,
        "plan_revision_id": first_id, "plan_content_hash": first_hash
    });
    let (status, body) = harness.approve(&run, stale).await;
    assert_eq!(status, StatusCode::CONFLICT, "stale plan approval: {body}");
    assert_eq!(
        harness.tree(&run).await["run"]["status"],
        "waiting_for_gate"
    );

    // Portable exports carry the exact plan + packet identity and history.
    let (status, headers, exported) = call_raw(
        &harness.app,
        &format!(
            "/v1/pipeline/runs/{run}/plan/export?format=json&revision={}",
            revision["id"].as_str().unwrap()
        ),
        Some(&harness.token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "plan export: {exported}");
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
            .contains("pln_")
    );
    let exported: Value = serde_json::from_str(&exported).unwrap();
    assert_eq!(exported["plan"]["content_hash"], revision["content_hash"]);
    assert_eq!(exported["plan"]["packet_revision_id"], approved_packet_id);
    assert_eq!(exported["history"].as_array().unwrap().len(), 2);
    let (status, headers, markdown) = call_raw(
        &harness.app,
        &format!(
            "/v1/pipeline/runs/{run}/plan/export?format=markdown&revision={}",
            revision["id"].as_str().unwrap()
        ),
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
    assert!(markdown.contains(revision["content_hash"].as_str().unwrap()));
    assert!(markdown.contains(&approved_packet_hash));
    assert!(markdown.contains("Depends on"));

    // Ready-task contract before approval: T-1 is ready, T-2 is blocked by
    // its dependency, and selection is refused until the exact plan is approved.
    let (status, tasks) = call(
        &harness.app,
        Method::GET,
        &format!(
            "/v1/pipeline/runs/{run}/plan/tasks?revision={}",
            revision["id"].as_str().unwrap()
        ),
        Some(&harness.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        tasks["ready_task_ids"]
            .as_array()
            .unwrap()
            .contains(&json!("T-1"))
    );
    assert!(
        !tasks["ready_task_ids"]
            .as_array()
            .unwrap()
            .contains(&json!("T-2"))
    );
    let selection = json!({
        "plan_revision_id": revision["id"], "plan_content_hash": revision["content_hash"], "task_id": "T-1"
    });
    let (status, body) = call(
        &harness.app,
        Method::POST,
        &format!("/v1/pipeline/runs/{run}/plan/select-task"),
        Some(&harness.token),
        Some(selection.clone()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "unapproved plan selection: {body}"
    );

    let approved = json!({
        "approved": true, "gate_instance_id": refreshed["run"]["gate_instance_id"],
        "expected_state_version": refreshed["run"]["state_version"],
        "idempotency_key": "exact-plan-approval",
        "packet_revision_id": approved_packet_id, "packet_content_hash": approved_packet_hash,
        "plan_revision_id": revision["id"], "plan_content_hash": revision["content_hash"]
    });
    let (status, receipt) = harness.approve(&run, approved).await;
    assert_eq!(status, StatusCode::OK, "plan approval: {receipt}");
    assert_eq!(receipt["plan_revision_id"], revision["id"]);
    assert_eq!(receipt["packet_revision_id"], approved_packet_id);
    let approved_plan = harness.plan(&run).await;
    assert_eq!(approved_plan["approved"]["id"], revision["id"]);
    assert_eq!(
        approved_plan["reviews"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|review| review["outcome"] == "approved")
            .count(),
        1
    );
    let final_tree = harness
        .wait_for(&run, |tree| tree["run"]["status"] == "completed")
        .await;
    assert_eq!(model.request_count(), 14, "approval never calls the model");
    assert!(
        !final_tree["flows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|flow| flow["flow_name"] == "build")
    );

    // Engineering selection now works from the exact approved plan and retains
    // criteria and provenance. A wrong hash or a blocked task conflicts.
    let (status, selected) = call(
        &harness.app,
        Method::POST,
        &format!("/v1/pipeline/runs/{run}/plan/select-task"),
        Some(&harness.token),
        Some(selection.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "task selection: {selected}");
    assert_eq!(selected["payload"]["task"]["id"], "T-1");
    assert_eq!(
        selected["payload"]["plan"]["content_hash"],
        revision["content_hash"]
    );
    assert_eq!(
        selected["payload"]["packet"]["content_hash"],
        approved_packet_hash
    );
    assert!(
        !selected["payload"]["task"]["acceptance_criteria"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        !selected["payload"]["task"]["verification"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let (status, replay) = call(
        &harness.app,
        Method::POST,
        &format!("/v1/pipeline/runs/{run}/plan/select-task"),
        Some(&harness.token),
        Some(selection),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["selection"]["id"], selected["selection"]["id"]);
    let blocked_selection = json!({
        "plan_revision_id": revision["id"], "plan_content_hash": revision["content_hash"], "task_id": "T-2"
    });
    let (status, body) = call(
        &harness.app,
        Method::POST,
        &format!("/v1/pipeline/runs/{run}/plan/select-task"),
        Some(&harness.token),
        Some(blocked_selection),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "blocked task selection: {body}"
    );
    // A caller cannot declare an unmet dependency complete. Selection rejects
    // any caller-supplied completed set because this slice has no durable
    // cross-task completion ledger.
    let forged_completion = json!({
        "plan_revision_id": revision["id"], "plan_content_hash": revision["content_hash"],
        "task_id": "T-2", "completed_task_ids": ["T-1"]
    });
    let (status, body) = call(
        &harness.app,
        Method::POST,
        &format!("/v1/pipeline/runs/{run}/plan/select-task"),
        Some(&harness.token),
        Some(forged_completion),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "forged completion: {body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("durable completed-task evidence"),
        "{body}"
    );
    let stale_selection = json!({
        "plan_revision_id": revision["id"], "plan_content_hash": first_hash, "task_id": "T-1"
    });
    let (status, _) = call(
        &harness.app,
        Method::POST,
        &format!("/v1/pipeline/runs/{run}/plan/select-task"),
        Some(&harness.token),
        Some(stale_selection),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    harness.shutdown().await;
}

#[tokio::test]
async fn invalid_plan_is_rejected_deterministically_before_any_human_gate() {
    let model = ScriptedModel::start().await;
    let harness = Harness::start(&model).await;
    model.push(packet_response("artifact:collect:collector"));
    model.push(packet_response("artifact:collect:collector"));
    let run = harness.create_run("Invalid plan gate").await;
    let tree = harness
        .wait_for(&run, |tree| {
            tree["run"]["status"] == "waiting_for_gate"
                && tree["run"]["gate_name"] == "collect-gate"
        })
        .await;
    let packet = harness.packet(&run).await;
    model.push(planning_response("survey"));
    model.push(planning_response("outline"));
    model.push(planning_response("sketch"));
    model.push(planning_response("align"));
    let mut invalid: Value =
        serde_json::from_str(include_str!("../../tj-plan/fixtures/complete.json")).unwrap();
    invalid["tasks"][0]["requirement_refs"] = json!(["REQ-INVENTED"]);
    model.push(text_response(
        &json!({
            "verdict": "success", "summary": "invalid plan", "content": "# Plan", "plan": invalid
        })
        .to_string(),
    ));
    model.push(planning_response("check"));
    let (status, _) = harness.approve(&run, approve_packet(&tree, &packet)).await;
    assert_eq!(status, StatusCode::OK);

    let tree = harness
        .wait_for(&run, |tree| tree["run"]["status"] == "failed")
        .await;
    assert_eq!(tree["run"]["status"], "failed");
    let (status, events) = call(
        &harness.app,
        Method::GET,
        &format!("/v1/pipeline/runs/{run}/events"),
        Some(&harness.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let planning_error = events
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|event| event["summary"].as_str())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        planning_error.contains("deterministic plan validation failed"),
        "{planning_error}"
    );
    assert!(planning_error.contains("REQ-INVENTED"), "{planning_error}");
    assert!(
        planning_error.contains("requirements packet revision"),
        "{planning_error}"
    );
    assert_eq!(
        harness.plan(&run).await["latest"],
        Value::Null,
        "invalid plan must not be persisted"
    );
    assert_ne!(
        harness.tree(&run).await["run"]["status"],
        "waiting_for_gate"
    );
    harness.shutdown().await;
}
