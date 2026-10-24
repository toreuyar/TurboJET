//! TJ-018: integrated lifecycle failure-injection harness.
//!
//! Every case goes through the real Axum API, Manager actor, Flow and Stage,
//! with a scripted OpenAI-compatible model server, disposable SQLite database
//! and disposable workspace. No live provider/forge/network effect is used.
//! The scripted responses are the fixture seed; run events/state are the trace.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, request::Builder};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tj_api::handlers::build_app;
use tj_db::RunStatus;
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
                            let mut buf = vec![0u8; 65536];
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
        "id": "chatcmpl-harness",
        "object": "chat.completion",
        "model": "scripted-model",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": text}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7}
    })
}

fn tool_response(name: &str, args: Value) -> Value {
    json!({
        "id": "chatcmpl-harness-tool",
        "object": "chat.completion",
        "model": "scripted-model",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{"id": format!("call-{name}"), "type": "function",
                    "function": {"name": name, "arguments": args.to_string()}}]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7}
    })
}

fn verdict(value: &str) -> Value {
    tool_response(
        "verdict",
        json!({"verdict": value, "content": format!("result {value}"), "summary": "scripted"}),
    )
}

fn question(text: &str) -> Value {
    tool_response(
        "question",
        json!({
            "question": text,
            "options": [{"id": "a", "label": "A"}, {"id": "b", "label": "B"}],
            "multi_select": false,
            "free_form": true
        }),
    )
}

struct Harness {
    _temp: TempDir,
    manager: ManagerHandle,
    app: axum::Router,
    token: String,
    project_id: String,
    lane_id: String,
}

impl Harness {
    async fn start(model: &ScriptedModel, budgets: Value) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let config_dir = root.join("config");
        let agent_dir = config_dir.join("stages/start");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join("prompt.md"), "Do the scripted work.").unwrap();
        std::fs::write(
            agent_dir.join("schema.json"),
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
                "id": "start", "name": "Start", "prompt": "prompt.md",
                "schema": "config/stages/start/schema.json", "timeout": 20,
                "max_output_retries": 3,
                "tools": {"read": true, "write": true, "edit": false, "bash": false}
            })
            .to_string(),
        )
        .unwrap();
        let workflow = json!({
            "version": 1, "name": "harness", "catalog": {"id": "synthetic-harness"}, "maturity": "experimental",
            "description": "TJ-018 harness", "entry": "flow-a", "exit": "flow-b",
            "limits": {"maxIterations": 2, "maxSteps": 8, "maxDepth": 3},
            "flow-order": ["flow-a", "gate-a", "flow-b"],
            "flows": {
                "flow-a": {"type": "flow", "entry": "start", "exit": "start", "stages": {
                    "start": {"agent": "config/stages/start/agent.json", "interactive": true,
                        "routes": {"success": "_next-flow"}}
                }},
                "gate-a": {"type": "gate", "routes": {"approved": "_next-flow", "rejected": "_rework"}, "stages": {}},
                "flow-b": {"type": "flow", "entry": "start", "exit": "start", "stages": {
                    "start": {"agent": "config/stages/start/agent.json", "interactive": true,
                        "routes": {"success": "_exit"}}
                }}
            }
        });
        std::fs::write(config_dir.join("graph.json"), workflow.to_string()).unwrap();
        let key_env = format!("TJ_HARNESS_KEY_{}", uuid::Uuid::new_v4().simple());
        // Safety: the key is unique to this harness and set before any worker
        // reads it. Test processes run one harness per test function.
        unsafe {
            std::env::set_var(&key_env, "unused-test-key");
        }
        std::fs::write(
            config_dir.join("fixture.json"),
            json!({
                "listen_addr": "127.0.0.1:0",
                "log_level": "warn",
                "database_path": root.join("state.db").to_string_lossy(),
                "workspace_base": root.join("workspaces").to_string_lossy(),
                "smg_path": config_dir.join("graph.json").to_string_lossy(),
                "adc_dir": config_dir.join("stages").to_string_lossy(),
                "providers": [{"id": "scripted", "type": "open_ai", "api_key_env": key_env,
                    "base_url": model.base_url(), "default_model": "scripted-model"}],
                "agents": [{"id": "chief", "provider_id": "scripted", "model": "scripted-model",
                    "default": true, "enabled": true, "tools": [], "system_prompt": "fixture"}],
                "budgets": budgets,
                "run_timeout_secs": 60
            })
            .to_string(),
        )
        .unwrap();

        let loader = tj_config::ConfigLoader::from_file(&config_dir.join("fixture.json")).unwrap();
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
            Some(json!({"name": "Harness", "catalog_id": "synthetic-harness"})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "project: {project}");
        let project_id = project["project"]["id"].as_str().unwrap().to_string();
        let lane_id = project["lane"]["id"].as_str().unwrap().to_string();
        Self {
            _temp: temp,
            manager,
            app,
            token,
            project_id,
            lane_id,
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
            "run {run_id} did not reach the expected state; tree={}",
            self.tree(run_id).await
        );
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
        Some(body) => {
            builder = builder.header("Content-Type", "application/json");
            builder.body(Body::from(body.to_string())).unwrap()
        }
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

fn default_budgets() -> Value {
    json!({
        "question_rounds_per_stage": 6, "question_rounds_per_run": 12,
        "model_calls": 20, "tool_calls": 20, "output_corrections": 4,
        "interaction_nudges": 4, "active_secs": 120
    })
}

#[tokio::test]
async fn api_gate_route_is_durable_idempotent_and_reconciled_without_double_execution() {
    let model = ScriptedModel::start().await;
    let harness = Harness::start(&model, default_budgets()).await;

    model.push(verdict("success"));
    let run1 = harness.create_run("gate happy path").await;
    let tree = harness
        .wait_for(&run1, |tree| tree["run"]["status"] == "waiting_for_gate")
        .await;
    assert_eq!(
        model.request_count(),
        1,
        "one stage model call before the gate"
    );
    assert_eq!(tree["run"]["gate_binding"]["kind"], "artifact");
    let gate_instance = tree["run"]["gate_instance_id"]
        .as_str()
        .unwrap()
        .to_string();
    let version = tree["run"]["state_version"].as_i64().unwrap();

    let budget_path = format!("/v1/pipeline/runs/{run1}/budget");
    let (status, budget) = call(
        &harness.app,
        Method::GET,
        &budget_path,
        Some(&harness.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(budget["model_calls_used"], 1);
    assert_eq!(budget["question_rounds_used"], 0);
    assert!(budget["model_calls_remaining"].as_u64().unwrap() >= 19);

    let (status, output) = call(
        &harness.app,
        Method::GET,
        &format!("/v1/pipeline/runs/{run1}/output"),
        Some(&harness.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "output: {output}");
    assert_eq!(output["gate_binding"]["kind"], "artifact");
    let output_id = output["gate_binding"]["artifact_id"]
        .as_str()
        .expect("generic gate binds an exact artifact")
        .to_string();
    let output_hash = output["gate_binding"]["content_hash"]
        .as_str()
        .unwrap()
        .to_string();
    let output_stage = output["gate_binding"]["stage_name"]
        .as_str()
        .unwrap()
        .to_string();

    model.push(verdict("success"));
    let body = json!({"approved": true, "gate_instance_id": gate_instance,
        "expected_state_version": version, "idempotency_key": "gate-key-1",
        "output_artifact_id": output_id, "output_content_hash": output_hash,
        "output_stage_name": output_stage});
    let (status, receipt) = call(
        &harness.app,
        Method::POST,
        &format!("/v1/pipeline/runs/{run1}/approve"),
        Some(&harness.token),
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "approve: {receipt}");
    assert_eq!(receipt["replayed"], false);
    assert_eq!(receipt["gate_binding_kind"], "artifact");
    harness
        .wait_for(&run1, |tree| tree["run"]["status"] == "completed")
        .await;
    assert_eq!(
        model.request_count(),
        2,
        "post-gate flow executes exactly once"
    );

    // Duplicate API submission replays the durable receipt and cannot spawn a
    // second flow or call the model again.
    let (status, replay) = call(
        &harness.app,
        Method::POST,
        &format!("/v1/pipeline/runs/{run1}/approve"),
        Some(&harness.token),
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(replay["replayed"].as_bool().unwrap());
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert_eq!(model.request_count(), 2);
    let tree = harness.tree(&run1).await;
    assert_eq!(
        tree["flows"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|flow| flow["flow_name"] == "flow-b")
            .count(),
        1
    );

    // Crash injection: the accepted decision commits durably while the manager
    // never routes it. Startup/effect reconciliation must route once, not replay
    // the completed flow.
    model.push(verdict("success"));
    let run2 = harness.create_run("gate crash").await;
    let tree = harness
        .wait_for(&run2, |tree| tree["run"]["status"] == "waiting_for_gate")
        .await;
    assert_eq!(model.request_count(), 3);
    let gate_instance = tree["run"]["gate_instance_id"]
        .as_str()
        .unwrap()
        .to_string();
    let version = tree["run"]["state_version"].as_i64().unwrap();
    let admin = harness
        .manager
        .db()
        .user_for_token(&harness.token)
        .unwrap()
        .unwrap();
    let crash_binding = harness
        .manager
        .db()
        .gate_output_binding(&gate_instance)
        .unwrap()
        .expect("generic gate persisted exact output binding");
    let decision = tj_db::GateDecision {
        id: String::new(),
        run_id: tj_db::RunId(run2.clone()),
        gate_name: String::new(),
        actor_id: admin.id.clone(),
        actor_name: "Admin".into(),
        approved: true,
        reason: None,
        note: None,
        created_at: chrono::Utc::now().to_rfc3339(),
        gate_instance_id: Some(gate_instance),
        idempotency_key: Some("gate-crash-key".into()),
        expected_state_version: Some(version),
        state_version: None,
        disposition: None,
        packet_revision_id: None,
        packet_content_hash: None,
        plan_revision_id: None,
        plan_content_hash: None,
        output_artifact_id: Some(crash_binding.artifact_id.clone()),
        output_content_hash: Some(crash_binding.content_hash.clone()),
        output_stage_name: Some(crash_binding.stage_name.clone()),
        acknowledge_incomplete: false,
    };
    let receipt = harness
        .manager
        .db()
        .decide_gate(&decision, version, 3)
        .unwrap();
    assert!(
        receipt.effect_id.is_some(),
        "gate decision must carry a durable routing effect"
    );
    let effect_id = receipt.effect_id.clone().unwrap();
    // No manager routing happened yet; assert the durable pending effect.
    assert_eq!(
        harness
            .manager
            .db()
            .load_pending_effect(&effect_id)
            .unwrap()
            .unwrap()
            .status,
        tj_db::EffectStatus::Pending
    );
    model.push(verdict("success"));
    harness
        .manager
        .sender()
        .send(tj_manager::ManagerEvent::ReconcilePendingEffects {
            run_id: run2.clone(),
        })
        .unwrap();
    harness
        .wait_for(&run2, |tree| tree["run"]["status"] == "completed")
        .await;
    assert_eq!(
        model.request_count(),
        4,
        "reconciliation starts the post-gate flow exactly once"
    );
    let tree = harness.tree(&run2).await;
    assert_eq!(
        tree["flows"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|flow| flow["flow_name"] == "flow-b")
            .count(),
        1
    );
    assert_eq!(
        harness
            .manager
            .db()
            .load_pending_effect(&effect_id)
            .unwrap()
            .unwrap()
            .status,
        tj_db::EffectStatus::Completed
    );
    let budget = harness.manager.budget(&run2).unwrap().unwrap();
    assert_eq!(budget.model_calls_used, 2);
    harness.shutdown().await;
}

#[tokio::test]
async fn durable_question_answer_and_question_budget_exhaustion_are_visible_through_the_api() {
    let model = ScriptedModel::start().await;
    let mut budgets = default_budgets();
    budgets["question_rounds_per_stage"] = json!(1);
    budgets["question_rounds_per_run"] = json!(2);
    let harness = Harness::start(&model, budgets).await;

    model.push(question("Which scope?"));
    model.push(question("One more question?"));
    let run = harness.create_run("question budget").await;

    // Wait for the durable open question (not just a log row).
    let open = loop {
        let (status, body) = call(
            &harness.app,
            Method::GET,
            &format!("/v1/pipeline/runs/{run}/questions?status=open"),
            Some(&harness.token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        if let Some(question) = body["questions"]
            .as_array()
            .and_then(|items| items.first())
            .cloned()
        {
            break question;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    let question_id = open["id"].as_str().unwrap();
    let stage_id = open["stage_id"].as_str().unwrap();
    let attempt = open["attempt"].as_u64().unwrap();
    let (status, receipt) = call(
        &harness.app,
        Method::POST,
        &format!("/v1/pipeline/runs/{run}/input"),
        Some(&harness.token),
        Some(
            json!({"text": "{\"selected\":[\"a\"]}", "question_id": question_id,
            "stage_id": stage_id, "attempt": attempt, "idempotency_key": "answer-key-1"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "answer: {receipt}");
    assert_eq!(receipt["status"], "accepted");

    let tree = harness
        .wait_for(&run, |tree| tree["run"]["status"] == "failed")
        .await;
    let stage = tree["flows"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|flow| flow["stages"].as_array().unwrap())
        .find(|stage| stage["stage_name"] == "start")
        .unwrap();
    assert!(
        stage["error"]
            .as_str()
            .unwrap()
            .contains("budget exhausted: question_rounds_per_stage"),
        "stage error: {}",
        stage["error"]
    );
    assert_eq!(
        model.request_count(),
        2,
        "the denied second question never reaches the model"
    );
    let (_, all) = call(
        &harness.app,
        Method::GET,
        &format!("/v1/pipeline/runs/{run}/questions?status=all"),
        Some(&harness.token),
        None,
    )
    .await;
    assert_eq!(all["questions"][0]["status"], "answered");
    let budget = harness.manager.budget(&run).unwrap().unwrap();
    assert_eq!(budget.question_rounds_used, 1);
    assert!(budget.exhausted);
    assert!(
        budget
            .exhausted_reason
            .as_deref()
            .unwrap()
            .contains("question_rounds_per_stage")
    );
    harness.shutdown().await;
}

#[tokio::test]
async fn cancel_while_waiting_for_a_question_stops_execution_and_fences_late_answers() {
    let model = ScriptedModel::start().await;
    let harness = Harness::start(&model, default_budgets()).await;
    model.push(question("Please answer before cancellation"));
    let run = harness.create_run("cancel while waiting").await;
    let open = loop {
        let (_, body) = call(
            &harness.app,
            Method::GET,
            &format!("/v1/pipeline/runs/{run}/questions?status=open"),
            Some(&harness.token),
            None,
        )
        .await;
        if let Some(question) = body["questions"]
            .as_array()
            .and_then(|items| items.first())
            .cloned()
        {
            break question;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    let (status, cancelled) = call(
        &harness.app,
        Method::POST,
        &format!("/v1/pipeline/runs/{run}/cancel"),
        Some(&harness.token),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "cancel: {cancelled}");
    assert_eq!(cancelled["status"], "cancelled");
    let requests_after_cancel = model.request_count();
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    assert_eq!(
        model.request_count(),
        requests_after_cancel,
        "no model call after cancellation"
    );
    let (status, late) = call(
        &harness.app,
        Method::POST,
        &format!("/v1/pipeline/runs/{run}/input"),
        Some(&harness.token),
        Some(
            json!({"text": "{\"selected\":[\"a\"]}", "question_id": open["id"],
            "stage_id": open["stage_id"], "attempt": open["attempt"], "idempotency_key": "late"}),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "late answer must conflict: {late}"
    );
    let (_, all) = call(
        &harness.app,
        Method::GET,
        &format!("/v1/pipeline/runs/{run}/questions?status=all"),
        Some(&harness.token),
        None,
    )
    .await;
    assert_eq!(all["questions"][0]["status"], "cancelled");
    harness.shutdown().await;
}

#[tokio::test]
async fn interrupted_question_is_preserved_then_explicit_api_retry_starts_one_new_attempt() {
    let model = ScriptedModel::start().await;
    let harness = Harness::start(&model, default_budgets()).await;
    model.push(question("Pending before interruption"));
    let run = harness.create_run("interrupted question").await;
    let open = loop {
        let (_, body) = call(
            &harness.app,
            Method::GET,
            &format!("/v1/pipeline/runs/{run}/questions?status=open"),
            Some(&harness.token),
            None,
        )
        .await;
        if let Some(question) = body["questions"]
            .as_array()
            .and_then(|items| items.first())
            .cloned()
        {
            break question;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    // Simulate a process crash by fencing the durable run directly, then stop
    // the local execution owner (the real startup code does this automatically).
    harness
        .manager
        .db()
        .mark_run_interrupted(&run, "injected crash", false)
        .unwrap();
    harness
        .manager
        .sender()
        .send(tj_manager::ManagerEvent::RunDeleted {
            run_id: run.clone(),
        })
        .unwrap();
    let tree = harness.tree(&run).await;
    assert_eq!(tree["run"]["status"], "interrupted");
    let (status, recovery) = call(
        &harness.app,
        Method::GET,
        &format!("/v1/pipeline/runs/{run}/recovery"),
        Some(&harness.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(recovery["recovery"]["resume_flow"], "flow-a");
    let (_, all) = call(
        &harness.app,
        Method::GET,
        &format!("/v1/pipeline/runs/{run}/questions?status=all"),
        Some(&harness.token),
        None,
    )
    .await;
    assert_eq!(
        all["questions"][0]["status"], "open",
        "pending question stays visible while interrupted"
    );
    assert_eq!(all["questions"][0]["id"], open["id"]);

    model.push(verdict("success"));
    let (status, retried) = call(
        &harness.app,
        Method::POST,
        &format!("/v1/pipeline/runs/{run}/retry"),
        Some(&harness.token),
        Some(json!({"acknowledge_unknown_effects": false})),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "retry: {retried}");
    assert_eq!(retried["plan"]["attempt_offset"], 1);
    let tree = harness
        .wait_for(&run, |tree| {
            tree["run"]["status"] == "waiting_for_gate" || tree["run"]["status"] == "completed"
        })
        .await;
    assert_eq!(tree["run"]["status"], "waiting_for_gate");
    let (_, all) = call(
        &harness.app,
        Method::GET,
        &format!("/v1/pipeline/runs/{run}/questions?status=all"),
        Some(&harness.token),
        None,
    )
    .await;
    assert_eq!(
        all["questions"][0]["status"], "cancelled",
        "old question stays durable but cannot be answered by the new attempt"
    );
    let flows = tree["flows"].as_array().unwrap();
    assert_eq!(
        flows
            .iter()
            .filter(|flow| flow["flow_name"] == "flow-a")
            .count(),
        2,
        "explicit retry creates exactly one new flow attempt"
    );
    assert_eq!(model.request_count(), 2);
    harness.shutdown().await;
}

#[tokio::test]
async fn startup_reconciles_a_pending_gate_route_and_marks_plain_running_runs_interrupted() {
    let model = ScriptedModel::start().await;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let config_dir = root.join("config");
    let agent_dir = config_dir.join("stages/start");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(agent_dir.join("prompt.md"), "Do work.").unwrap();
    std::fs::write(
        agent_dir.join("schema.json"),
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
            "id": "start", "name": "Start", "prompt": "prompt.md",
            "schema": "config/stages/start/schema.json", "timeout": 20,
            "tools": {"read": true, "write": false, "edit": false, "bash": false}
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(config_dir.join("graph.json"), json!({
        "version": 1, "name": "startup-harness", "catalog": {"id": "synthetic-startup-harness"}, "maturity": "experimental",
        "entry": "flow-a", "exit": "flow-b",
        "limits": {"maxIterations": 2, "maxSteps": 8, "maxDepth": 3},
        "flow-order": ["flow-a", "gate-a", "flow-b"],
        "flows": {
            "flow-a": {"type": "flow", "entry": "start", "exit": "start", "stages": {
                "start": {"agent": "config/stages/start/agent.json", "interactive": true,
                    "routes": {"success": "_next-flow"}}
            }},
            "gate-a": {"type": "gate", "routes": {"approved": "_next-flow", "rejected": "_rework"}, "stages": {}},
            "flow-b": {"type": "flow", "entry": "start", "exit": "start", "stages": {
                "start": {"agent": "config/stages/start/agent.json", "interactive": true,
                    "routes": {"success": "_exit"}}
            }}
        }
    }).to_string()).unwrap();
    let key_env = format!("TJ_STARTUP_KEY_{}", uuid::Uuid::new_v4().simple());
    unsafe {
        std::env::set_var(&key_env, "unused-test-key");
    }
    let db_path = root.join("state.db");
    std::fs::write(
        config_dir.join("fixture.json"),
        json!({
            "log_level": "warn",
            "database_path": db_path.to_string_lossy(),
            "workspace_base": root.join("workspaces").to_string_lossy(),
            "smg_path": config_dir.join("graph.json").to_string_lossy(),
            "adc_dir": config_dir.join("stages").to_string_lossy(),
            "providers": [{"id": "scripted", "type": "open_ai", "api_key_env": key_env,
                "base_url": model.base_url(), "default_model": "scripted-model"}],
            "agents": [{"id": "chief", "provider_id": "scripted", "model": "scripted-model",
                "default": true, "enabled": true, "tools": [], "system_prompt": "fixture"}],
            "budgets": default_budgets(),
            "run_timeout_secs": 60
        })
        .to_string(),
    )
    .unwrap();

    // Seed the previous process's last visible state: a completed work flow,
    // an active gate, and a decision accepted durably but never routed.
    let db = tj_db::Database::open(&db_path.to_string_lossy()).unwrap();
    db.create_user(&tj_db::User {
        id: "admin".into(),
        username: "admin".into(),
        password_hash: "unused".into(),
        display_name: None,
        is_admin: true,
        created_at: String::new(),
    })
    .unwrap();
    let startup_loader =
        tj_config::ConfigLoader::from_file(&config_dir.join("fixture.json")).unwrap();
    let startup_graph: tj_config::smg::StateMachineGraph =
        serde_json::from_str(&std::fs::read_to_string(config_dir.join("graph.json")).unwrap())
            .unwrap();
    let startup_bundle = tj_config::bundle::ExecutionBundle::build(
        &startup_loader,
        "run-crash",
        None,
        &startup_graph,
    )
    .unwrap();
    let startup_bundle_record = tj_db::ExecutionBundleRecord {
        id: "bundle_run_crash".into(),
        run_id: tj_db::RunId("run-crash".into()),
        workflow_id: None,
        bundle_hash: startup_bundle.bundle_hash.clone(),
        bundle_json: serde_json::to_string(&startup_bundle).unwrap(),
        legacy: false,
        created_at: startup_bundle.created_at.clone(),
    };
    db.save_execution_bundle(&startup_bundle_record).unwrap();
    let mut crash_run = tj_db::PipelineRun::new(
        "run-crash".into(),
        None,
        None,
        "crash".into(),
        RunStatus::Running,
        None,
        None,
        Some("trace".into()),
        None,
    );
    crash_run.set_execution_bundle(
        Some(startup_bundle_record.id.clone()),
        Some(startup_bundle_record.bundle_hash.clone()),
    );
    db.save_run(&crash_run).unwrap();
    db.save_flow(&tj_db::FlowRecord::new(
        "flow-work".into(),
        tj_db::RunId("run-crash".into()),
        "flow-a".into(),
        Some(1),
        None,
        None,
    ))
    .unwrap();
    db.complete_flow("flow-work").unwrap();
    let version = db
        .enter_gate("run-crash", 0, "gate_crash", "gate-a", "review")
        .unwrap();
    let decision = tj_db::GateDecision {
        id: String::new(),
        run_id: tj_db::RunId("run-crash".into()),
        gate_name: String::new(),
        actor_id: "admin".into(),
        actor_name: "Admin".into(),
        approved: true,
        reason: None,
        note: None,
        created_at: chrono::Utc::now().to_rfc3339(),
        gate_instance_id: Some("gate_crash".into()),
        idempotency_key: Some("startup-key".into()),
        expected_state_version: Some(version),
        state_version: None,
        disposition: None,
        packet_revision_id: None,
        packet_content_hash: None,
        plan_revision_id: None,
        plan_content_hash: None,
        output_artifact_id: None,
        output_content_hash: None,
        output_stage_name: None,
        acknowledge_incomplete: false,
    };
    let receipt = db.decide_gate(&decision, version, 3).unwrap();
    assert!(receipt.effect_id.is_some());
    db.save_run(&tj_db::PipelineRun::new(
        "run-plain".into(),
        None,
        None,
        "plain".into(),
        RunStatus::Running,
        None,
        None,
        Some("trace2".into()),
        None,
    ))
    .unwrap();
    let unknown = db
        .create_pending_effect(
            "run-plain",
            "flow_completion",
            "flow_completion:dead-process",
            &serde_json::json!({"flow": "flow-a"}),
        )
        .unwrap();
    drop(db);

    model.push(verdict("success"));
    let loader = tj_config::ConfigLoader::from_file(&config_dir.join("fixture.json")).unwrap();
    let spawn = spawn_manager(loader).await.unwrap();
    let manager = spawn.handle.clone();
    let _app = build_app(spawn).await.unwrap();

    // The pure routing effect is reconciled; ordinary in-flight work is not
    // silently replayed.
    for _ in 0..200 {
        if manager
            .db()
            .load_run("run-crash")
            .unwrap()
            .unwrap()
            .status()
            == &RunStatus::Completed
            && manager
                .db()
                .load_run("run-plain")
                .unwrap()
                .unwrap()
                .status()
                == &RunStatus::Interrupted
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(
        *manager
            .db()
            .load_run("run-crash")
            .unwrap()
            .unwrap()
            .status(),
        RunStatus::Completed
    );
    assert_eq!(
        *manager
            .db()
            .load_run("run-plain")
            .unwrap()
            .unwrap()
            .status(),
        RunStatus::Interrupted
    );
    assert_eq!(
        model.request_count(),
        1,
        "only the post-gate flow runs, exactly once"
    );
    let flows = manager.db().load_flows_for_run("run-crash").unwrap();
    assert_eq!(
        flows
            .iter()
            .filter(|flow| flow.flow_name() == "flow-b")
            .count(),
        1
    );
    let recovery = manager.db().recovery_state("run-plain").unwrap().unwrap();
    assert!(recovery.reason.contains("explicit retry"));
    assert!(recovery.effects_unknown);
    assert_eq!(
        manager
            .db()
            .load_pending_effect(&unknown)
            .unwrap()
            .unwrap()
            .status,
        tj_db::EffectStatus::Ambiguous,
        "completion/publication effects are never replayed automatically"
    );
    let _ = manager.shutdown().await;
}

#[tokio::test]
async fn malformed_output_exhausts_the_separate_correction_budget_and_fails_closed() {
    let model = ScriptedModel::start().await;
    let mut budgets = default_budgets();
    budgets["output_corrections"] = json!(1);
    budgets["model_calls"] = json!(10);
    let harness = Harness::start(&model, budgets).await;
    model.push(tool_response("verdict", json!({"not": "a verdict"})));
    model.push(tool_response("verdict", json!({"still": "invalid"})));
    let run = harness.create_run("output contract").await;
    let tree = harness
        .wait_for(&run, |tree| tree["run"]["status"] == "failed")
        .await;
    let stage = tree["flows"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|flow| flow["stages"].as_array().unwrap())
        .find(|stage| stage["stage_name"] == "start")
        .unwrap();
    let error = stage["error"].as_str().unwrap();
    assert!(
        error.contains("budget exhausted: output_corrections"),
        "{error}"
    );
    let budget = harness.manager.budget(&run).unwrap().unwrap();
    assert_eq!(budget.output_corrections_used, 1);
    assert_eq!(budget.model_calls_used, 2);
    harness.shutdown().await;
}
