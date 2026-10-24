//! Integrated generic-workflow journeys.
//!
//! These tests drive the real Axum API → Manager → Flow → Stage → SQLite and
//! run-workspace path with a scripted OpenAI-compatible model. They prove the
//! three repository-free templates, checker correction, a user-addressed
//! clarification, human rejection/revision plus exact-output approval, and
//! deterministic citation verification. No Git, forge, SSH, shell, external
//! service or paid model is used.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
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
    bodies: Arc<Mutex<Vec<String>>>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl ScriptedModel {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let responses = Arc::new(Mutex::new(VecDeque::new()));
        let requests = Arc::new(AtomicUsize::new(0));
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let (shutdown, mut stop) = tokio::sync::oneshot::channel::<()>();
        let responses_for_server = Arc::clone(&responses);
        let requests_for_server = Arc::clone(&requests);
        let bodies_for_server = Arc::clone(&bodies);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((mut stream, _)) = accepted else { continue; };
                        let responses = Arc::clone(&responses_for_server);
                        let requests = Arc::clone(&requests_for_server);
                        let bodies = Arc::clone(&bodies_for_server);
                        tokio::spawn(async move {
                            use tokio::io::{AsyncReadExt, AsyncWriteExt};
                            let mut buf = vec![0u8; 262144];
                            let n = stream.read(&mut buf).await.unwrap_or(0);
                            bodies.lock().unwrap().push(String::from_utf8_lossy(&buf[..n]).to_string());
                            requests.fetch_add(1, Ordering::SeqCst);
                            let response = responses.lock().unwrap().pop_front()
                                .unwrap_or_else(|| text_response("{\"verdict\":\"success\",\"summary\":\"fallback\"}"));
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
            bodies,
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
    fn body(&self, index: usize) -> String {
        self.bodies
            .lock()
            .unwrap()
            .get(index)
            .cloned()
            .unwrap_or_default()
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
        "id": "chatcmpl-generic", "object": "chat.completion", "model": "scripted-model",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": text}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7}
    })
}

fn structured(value: Value) -> Value {
    text_response(&value.to_string())
}

fn tool_response(name: &str, args: Value) -> Value {
    json!({
        "id": "chatcmpl-generic-tool", "object": "chat.completion", "model": "scripted-model",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": null,
            "tool_calls": [{"id": format!("call-{name}"), "type": "function",
                "function": {"name": name, "arguments": args.to_string()}}]},
            "finish_reason": "tool_calls"}],
        "usage": {"prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7}
    })
}

fn write_output(text: &str) -> Value {
    tool_response("write_file", json!({"path": "output.md", "content": text}))
}

fn question(text: &str) -> Value {
    tool_response(
        "question",
        json!({
            "question": text,
            "options": [{"id": "a", "label": "Short"}, {"id": "b", "label": "Detailed"}],
            "multi_select": false,
            "free_form": true
        }),
    )
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

struct Harness {
    _temp: TempDir,
    manager: ManagerHandle,
    app: axum::Router,
    token: String,
    project_id: String,
    lane_id: String,
    project_workspace: PathBuf,
}

impl Harness {
    async fn start(model: &ScriptedModel, profile: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let config_dir = root.join("config");
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        // Copy only the self-contained packages. No shared agent/schema tree
        // exists, so any legacy fallback would fail.
        let workflows_dir = config_dir.join("workflows");
        for package in [
            "agent-loop",
            "clarify-produce-review",
            "research-synthesize-verify",
        ] {
            copy_dir(
                &repo_root.join("workflows").join(package),
                &workflows_dir.join(package),
            );
        }
        let key_env = format!("TJ_GENERIC_KEY_{}", uuid::Uuid::new_v4().simple());
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
                "workflow_sources": [{"id": "generic-examples", "path": workflows_dir.to_string_lossy(), "kind": "directory"}],
                "providers": [{"id": "scripted", "type": "open_ai", "api_key_env": key_env,
                    "base_url": model.base_url(), "default_model": "scripted-model"}],
                "agents": [{"id": "chief", "provider_id": "scripted", "model": "scripted-model",
                    "default": true, "enabled": true, "tools": [], "system_prompt": "test"}],
                "budgets": {"question_rounds_per_stage": 6, "question_rounds_per_run": 12,
                    "model_calls": 40, "tool_calls": 40, "output_corrections": 8,
                    "interaction_nudges": 6, "active_secs": 120},
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
        let (status, body) = call(
            &app,
            Method::POST,
            "/v1/projects",
            Some(&token),
            Some(json!({"name": format!("{profile} harness"), "catalog_id": profile})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "project: {body}");
        assert_eq!(body["project"]["profile"], profile);
        assert_eq!(body["project"]["provisioning_state"], "not_required");
        let project_id = body["project"]["id"].as_str().unwrap().to_string();
        let lane_id = body["lane"]["id"].as_str().unwrap().to_string();
        let project_workspace = PathBuf::from(body["project"]["workspace_path"].as_str().unwrap());
        assert!(!project_workspace.join(".git").exists());
        Self {
            _temp: temp,
            manager,
            app,
            token,
            project_id,
            lane_id,
            project_workspace,
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

    async fn output(&self, run_id: &str) -> Value {
        let (status, body) = call(
            &self.app,
            Method::GET,
            &format!("/v1/pipeline/runs/{run_id}/output"),
            Some(&self.token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "output: {body}");
        body
    }

    async fn wait_for<F>(&self, run_id: &str, predicate: F) -> Value
    where
        F: Fn(&Value) -> bool,
    {
        for _ in 0..300 {
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

fn plan_args() -> Value {
    json!({
        "verdict": "success",
        "summary": "Summarize the supplied notes",
        "approach": ["Read the notes", "Summarize the decisions"],
        "criteria": ["Checkable criterion one", "Checkable criterion two"],
        "unknowns": [],
        "needs_input": [],
        "content": "# Plan\n\nSummarize the supplied notes."
    })
}

fn act_answer(label: &str) -> Value {
    json!({
        "verdict": "produced",
        "summary": format!("draft {label}"),
        "draft": format!("Draft {label}: invite-only beta with unchanged plans."),
        "feedback_applied": if label == "revised" { json!(["Please add a concrete example"]) } else { json!([]) },
        "unknowns": [],
        "needs_input": [],
        "content": format!("# Draft {label}\n\nInvite-only beta. Existing plans stay unchanged.")
    })
}

fn check_answer(verdict: &str, feedback: Value) -> Value {
    json!({
        "verdict": verdict,
        "summary": format!("check {verdict}"),
        "feedback": feedback,
        "criteria_results": ["criterion one checked"],
        "unknowns": [],
        "content": format!("# Check\n\n{}", verdict)
    })
}

fn result_answer() -> Value {
    json!({
        "verdict": "success",
        "summary": "Final summary",
        "answer": "Invite-only beta. Existing plans stay unchanged.",
        "unknowns": [],
        "needs_input": [],
        "content": "# Result\n\nInvite-only beta. Existing plans stay unchanged."
    })
}

fn clarification_args(revised: bool) -> Value {
    json!({
        "verdict": "success",
        "summary": "Produce a short launch note",
        "clarification": "Short launch note for the team.",
        "output_contract": "One short markdown launch note.",
        "unknowns": [],
        "content": if revised { "# Clarification\n\nRevise the note using the review cause." } else { "# Clarification\n\nShort launch note." }
    })
}

fn produce_answer(revised: bool) -> Value {
    json!({
        "verdict": "produced",
        "summary": if revised { "revised launch note" } else { "launch note" },
        "deliverable": if revised { "Launch note v2 with security review called out." } else { "Launch note v1." },
        "assumptions": [],
        "unknowns": [],
        "revision_notes": if revised { json!(["Add the security review caveat"]) } else { json!([]) },
        "content": if revised { "# Launch note v2\n\nSecurity review is still open." } else { "# Launch note v1\n\nInvite-only beta starts soon." }
    })
}

fn research_answer(found: bool) -> Value {
    if found {
        json!({
            "verdict": "found",
            "summary": "Found the supplied pricing evidence",
            "evidence": [{"path": "inputs/option-a.md", "excerpt": "Monthly cost starts at 400 EUR", "claim": "Cost"}],
            "gaps": [],
            "content": "# Evidence\n\nOption A costs 400 EUR per month."
        })
    } else {
        json!({
            "verdict": "missing",
            "summary": "No usable evidence",
            "evidence": [],
            "gaps": ["No input document describes the requested topic"],
            "content": "# Evidence\n\nNo usable documents."
        })
    }
}

fn synthesize_answer() -> Value {
    json!({
        "verdict": "drafted",
        "summary": "Compared from supplied documents",
        "answer": "Option A starts at 400 EUR per month.",
        "citations": [{"path": "inputs/option-a.md", "excerpt": "Monthly cost starts at 400 EUR"}],
        "gaps": [],
        "content": "# Answer\n\nOption A starts at 400 EUR per month."
    })
}

fn synthesize_fake() -> Value {
    json!({
        "verdict": "drafted",
        "summary": "Invented evidence",
        "answer": "The vendor guarantees a 10 EUR price.",
        "citations": [{"path": "inputs/missing.md", "excerpt": "guaranteed 10 EUR"}],
        "gaps": [],
        "content": "# Answer\n\nInvented proof."
    })
}

fn verify_answer(verdict: &str) -> Value {
    let citations = if verdict == "missing-evidence" {
        json!([])
    } else {
        json!([{"path": "inputs/option-a.md", "excerpt": "Monthly cost starts at 400 EUR"}])
    };
    json!({
        "verdict": verdict,
        "summary": format!("verify {verdict}"),
        "issues": if verdict == "verified" { json!([]) } else { json!(["need a source"]) },
        "citations": citations,
        "content": format!("# Verification\n\n{verdict}")
    })
}

fn verify_fake() -> Value {
    json!({
        "verdict": "verified",
        "summary": "claimed verified",
        "issues": [],
        "citations": [{"path": "inputs/missing.md", "excerpt": "guaranteed 10 EUR"}],
        "content": "# Verification\n\nclaimed"
    })
}

#[tokio::test]
async fn agent_loop_checker_feedback_reenters_act_and_completes_without_repository_effects() {
    let model = ScriptedModel::start().await;
    let harness = Harness::start(&model, "agent-loop").await;

    let (status, profiles) = call(
        &harness.app,
        Method::GET,
        "/v1/profiles",
        Some(&harness.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(profiles["profiles"].as_array().unwrap().len(), 3);
    let (status, workflows) = call(
        &harness.app,
        Method::GET,
        "/v1/workflows",
        Some(&harness.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let listed = workflows["workflows"].as_array().unwrap();
    assert!(
        listed
            .iter()
            .any(|wf| wf["catalog_id"] == "agent-loop" && wf["maturity"] == "experimental")
    );
    assert!(
        listed
            .iter()
            .any(|wf| wf["catalog_id"] == "clarify-produce-review")
    );
    assert!(
        listed
            .iter()
            .any(|wf| wf["catalog_id"] == "research-synthesize-verify")
    );
    assert_eq!(listed.len(), 3, "only the copied packages are configured");
    for workflow in listed {
        assert_eq!(
            workflow["source_kind"], "package",
            "generic templates must load as packages: {workflow}"
        );
        assert!(
            workflow["asset_count"].as_u64().unwrap_or(0) > 0,
            "package assets must be stored in the registry: {workflow}"
        );
        assert!(
            workflow["smg_path"]
                .as_str()
                .is_some_and(|path| path.ends_with("/workflow.smg.json")),
            "package source path must point at its manifest: {workflow}"
        );
    }

    model.push(write_output("# Plan\n\nSummarize the supplied notes."));
    model.push(tool_response("verdict", plan_args()));
    model.push(structured(act_answer("initial")));
    model.push(structured(check_answer(
        "revise",
        json!(["Please add a concrete example"]),
    )));
    model.push(structured(act_answer("revised")));
    model.push(structured(check_answer("pass", json!([]))));
    model.push(structured(result_answer()));

    let run = harness
        .create_run("Summarize the supplied launch notes")
        .await;
    let tree = harness
        .wait_for(&run, |tree| tree["run"]["status"] == "completed")
        .await;

    assert_eq!(
        model.request_count(),
        7,
        "plan write+verdict, act, check, revised act, check, result"
    );
    assert_eq!(tree["run"]["profile"], "agent-loop");
    assert_eq!(tree["flows"].as_array().unwrap().len(), 1);
    assert_eq!(tree["run"]["pr_url"], Value::Null);
    assert!(!harness.project_workspace.join(".git").exists());
    let run_workspace = harness.project_workspace.join("runs").join(&run);
    assert!(run_workspace.exists());
    assert!(!run_workspace.join(".git").exists());

    // The first act received the plan's criteria. The second act received the
    // checker's exact feedback and the previous draft.
    assert!(
        model.body(2).contains("Checkable criterion one"),
        "plan criteria must be handed to act"
    );
    let retry = model.body(4);
    assert!(
        retry.contains("Please add a concrete example"),
        "check feedback must re-enter act"
    );
    assert!(
        retry.contains("Draft initial"),
        "the previous draft must be available for revision"
    );

    harness.shutdown().await;
}

#[tokio::test]
async fn clarify_produce_review_addresses_user_input_revises_on_rejection_and_approves_exact_output()
 {
    let model = ScriptedModel::start().await;
    let harness = Harness::start(&model, "clarify-produce-review").await;

    model.push(question("How detailed should the launch note be?"));
    model.push(write_output("# Clarification\n\nShort launch note."));
    model.push(tool_response("verdict", clarification_args(false)));
    model.push(structured(produce_answer(false)));

    let run = harness.create_run("Write a launch note for the team").await;

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
    let (status, receipt) = call(
        &harness.app,
        Method::POST,
        &format!("/v1/pipeline/runs/{run}/input"),
        Some(&harness.token),
        Some(json!({
            "text": "{\"selected\":[\"a\"],\"text\":\"Short — one paragraph.\"}",
            "question_id": open["id"], "stage_id": open["stage_id"],
            "attempt": open["attempt"], "idempotency_key": "clarify-answer-1"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "answer: {receipt}");
    let first = harness
        .wait_for(&run, |tree| tree["run"]["status"] == "waiting_for_gate")
        .await;
    let answer_seen =
        (0..model.request_count()).any(|i| model.body(i).contains("Short — one paragraph."));
    assert!(answer_seen, "the accepted user answer must reach the model");
    let first_output = harness.output(&run).await;
    let first_binding = first_output["gate_binding"].clone();
    assert_eq!(first_binding["stage_name"], "produce");
    assert_eq!(first_binding["artifact_id"], first_output["latest"]["id"]);
    assert_eq!(
        first_binding["content_hash"],
        first_output["latest"]["content_hash"]
    );
    assert!(
        first_output["latest"]["content"]
            .as_str()
            .unwrap()
            .contains("Launch note v1")
    );

    let version = first["run"]["state_version"].as_i64().unwrap();
    let gate = first["run"]["gate_instance_id"]
        .as_str()
        .unwrap()
        .to_string();
    model.push(write_output(
        "# Clarification\n\nRevise with the review cause.",
    ));
    model.push(tool_response("verdict", clarification_args(true)));
    model.push(structured(produce_answer(true)));
    let (status, rejected) = call(
        &harness.app,
        Method::POST,
        &format!("/v1/pipeline/runs/{run}/approve"),
        Some(&harness.token),
        Some(json!({
            "approved": false, "reason": "Add the security review caveat",
            "gate_instance_id": gate, "expected_state_version": version,
            "idempotency_key": "reject-output-1",
            "output_artifact_id": first_binding["artifact_id"],
            "output_content_hash": first_binding["content_hash"],
            "output_stage_name": first_binding["stage_name"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reject: {rejected}");
    assert_eq!(rejected["disposition"], "rework");

    let second = harness
        .wait_for(&run, |tree| {
            tree["run"]["status"] == "waiting_for_gate"
                && tree["run"]["state_version"].as_i64().unwrap_or(0) > version
        })
        .await;
    let second_output = harness.output(&run).await;
    let second_binding = second_output["gate_binding"].clone();
    assert_ne!(first_binding["artifact_id"], second_binding["artifact_id"]);
    assert_ne!(
        first_binding["content_hash"],
        second_binding["content_hash"]
    );
    assert!(
        second_output["latest"]["content"]
            .as_str()
            .unwrap()
            .contains("Security review")
    );
    // The rework cause reached the clarify stage, and the revised deliverable
    // was produced before the new gate bound to its exact artifact.
    assert!(
        (0..model.request_count()).any(|i| model.body(i).contains("__rework")),
        "rework cause must reach the flow"
    );
    assert!(
        (0..model.request_count())
            .any(|i| model.body(i).contains("Add the security review caveat"))
    );
    assert!(
        second_output["latest"]["content"]
            .as_str()
            .unwrap()
            .contains("Launch note v2")
    );

    let version = second["run"]["state_version"].as_i64().unwrap();
    let gate = second["run"]["gate_instance_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, stale) = call(
        &harness.app,
        Method::POST,
        &format!("/v1/pipeline/runs/{run}/approve"),
        Some(&harness.token),
        Some(json!({
            "approved": true,
            "gate_instance_id": gate, "expected_state_version": version,
            "idempotency_key": "stale-output-approval",
            "output_artifact_id": first_binding["artifact_id"],
            "output_content_hash": first_binding["content_hash"],
            "output_stage_name": first_binding["stage_name"],
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "stale output identity must conflict: {stale}"
    );
    let (status, approved) = call(
        &harness.app,
        Method::POST,
        &format!("/v1/pipeline/runs/{run}/approve"),
        Some(&harness.token),
        Some(json!({
            "approved": true,
            "gate_instance_id": gate, "expected_state_version": version,
            "idempotency_key": "approve-output-2",
            "output_artifact_id": second_binding["artifact_id"],
            "output_content_hash": second_binding["content_hash"],
            "output_stage_name": second_binding["stage_name"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "approve: {approved}");
    assert_eq!(
        approved["output_artifact_id"],
        second_binding["artifact_id"]
    );
    assert_eq!(
        approved["output_content_hash"],
        second_binding["content_hash"]
    );
    harness
        .wait_for(&run, |tree| tree["run"]["status"] == "completed")
        .await;

    let decisions = call(
        &harness.app,
        Method::GET,
        &format!("/v1/pipeline/runs/{run}/decisions"),
        Some(&harness.token),
        None,
    )
    .await
    .1;
    assert_eq!(decisions[0]["approved"], false);
    assert_eq!(
        decisions[0]["output_artifact_id"],
        first_binding["artifact_id"]
    );
    assert_eq!(decisions[1]["approved"], true);
    assert_eq!(
        decisions[1]["output_artifact_id"],
        second_binding["artifact_id"]
    );
    assert!(!harness.project_workspace.join(".git").exists());

    harness.shutdown().await;
}

#[tokio::test]
async fn research_verifies_actual_citations_fails_invented_proof_and_bounds_missing_evidence_loops()
{
    let model = ScriptedModel::start().await;
    let harness = Harness::start(&model, "research-synthesize-verify").await;
    std::fs::create_dir_all(harness.project_workspace.join("inputs")).unwrap();
    std::fs::write(
        harness.project_workspace.join("inputs/option-a.md"),
        "# Option A\n\nMonthly cost starts at 400 EUR.\n",
    )
    .unwrap();

    // 1. A real citation completes, and the run workspace contains the copied
    //    input document.
    model.push(structured(research_answer(true)));
    model.push(structured(synthesize_answer()));
    model.push(structured(verify_answer("verified")));
    let run = harness
        .create_run("Compare the supplied options on cost")
        .await;
    harness
        .wait_for(&run, |tree| tree["run"]["status"] == "completed")
        .await;
    assert_eq!(model.request_count(), 3);
    let run_workspace = harness.project_workspace.join("runs").join(&run);
    assert!(run_workspace.join("inputs/option-a.md").exists());
    assert!(!run_workspace.join(".git").exists());
    assert!(!harness.project_workspace.join(".git").exists());

    // 2. A model that claims a non-existent citation cannot complete the run.
    model.push(structured(research_answer(true)));
    model.push(structured(synthesize_fake()));
    model.push(structured(verify_fake()));
    let fake = harness
        .create_run("Cite a source that does not exist")
        .await;
    let fake_tree = harness
        .wait_for(&fake, |tree| tree["run"]["status"] == "failed")
        .await;
    let events = call(
        &harness.app,
        Method::GET,
        &format!("/v1/pipeline/runs/{fake}/events"),
        Some(&harness.token),
        None,
    )
    .await
    .1;
    assert!(
        serde_json::to_string(&events)
            .unwrap()
            .contains("citation verification failed"),
        "invented proof must fail deterministically: {fake_tree}"
    );
    assert!(!harness.project_workspace.join(".git").exists());

    // 3. Missing evidence loops research/synthesis within maxSteps before a
    //    verified source-backed answer completes. Seven model calls exercise
    //    both back-edges, not a disconnected JSON fixture.
    model.push(structured(research_answer(false)));
    model.push(structured(research_answer(true)));
    model.push(structured(synthesize_answer()));
    model.push(structured(verify_answer("missing-evidence")));
    model.push(structured(research_answer(true)));
    model.push(structured(synthesize_answer()));
    model.push(structured(verify_answer("verified")));
    let looped = harness
        .create_run("Answer only from supplied documents")
        .await;
    harness
        .wait_for(&looped, |tree| tree["run"]["status"] == "completed")
        .await;
    assert_eq!(model.request_count(), 3 + 3 + 7);
    assert!(!harness.project_workspace.join(".git").exists());

    harness.shutdown().await;
}

#[tokio::test]
async fn generic_human_rejections_are_bounded_and_cancel_after_three_revisions() {
    let model = ScriptedModel::start().await;
    let harness = Harness::start(&model, "clarify-produce-review").await;

    model.push(question("How detailed should the note be?"));
    model.push(write_output("# Clarification\n\nShort note."));
    model.push(tool_response("verdict", clarification_args(false)));
    model.push(structured(produce_answer(false)));
    for _ in 0..2 {
        model.push(write_output("# Clarification\n\nRevise the note."));
        model.push(tool_response("verdict", clarification_args(true)));
        model.push(structured(produce_answer(true)));
    }

    let run = harness.create_run("Write a reviewed note").await;
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
    call(
        &harness.app,
        Method::POST,
        &format!("/v1/pipeline/runs/{run}/input"),
        Some(&harness.token),
        Some(
            json!({"text": "{\"selected\":[\"a\"],\"text\":\"One paragraph.\"}",
            "question_id": open["id"], "stage_id": open["stage_id"],
            "attempt": open["attempt"], "idempotency_key": "bound-answer"}),
        ),
    )
    .await;

    let mut last_version = 0;
    for rejection in 1..=3 {
        let tree = harness
            .wait_for(&run, |tree| {
                tree["run"]["status"] == "waiting_for_gate"
                    && tree["run"]["state_version"].as_i64().unwrap_or(0) > last_version
            })
            .await;
        // Always re-read the output after the gate is durable so the decision
        // names exactly the artifact the reviewer saw.
        let output = harness.output(&run).await;
        let binding = output["gate_binding"].clone();
        let version = tree["run"]["state_version"].as_i64().unwrap();
        last_version = version;
        let gate = tree["run"]["gate_instance_id"]
            .as_str()
            .unwrap()
            .to_string();
        let (status, receipt) = call(
            &harness.app,
            Method::POST,
            &format!("/v1/pipeline/runs/{run}/approve"),
            Some(&harness.token),
            Some(
                json!({"approved": false, "reason": format!("revision {rejection}"),
                "gate_instance_id": gate, "expected_state_version": version,
                "idempotency_key": format!("bounded-reject-{rejection}"),
                "output_artifact_id": binding["artifact_id"],
                "output_content_hash": binding["content_hash"],
                "output_stage_name": binding["stage_name"]}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "reject {rejection}: {receipt}");
        if rejection == 3 {
            assert_eq!(receipt["disposition"], "rework_limit_cancelled");
            assert_eq!(receipt["status"], "cancelled");
        } else {
            assert_eq!(receipt["disposition"], "rework");
        }
    }

    let tree = harness
        .wait_for(&run, |tree| tree["run"]["status"] == "cancelled")
        .await;
    assert_eq!(
        tree["flows"].as_array().unwrap().len(),
        3,
        "no fourth flow after the bound"
    );
    assert_eq!(
        model.request_count(),
        10,
        "three clarify/produce attempts only"
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
    assert_eq!(decisions.as_array().unwrap().len(), 3);
    assert_eq!(decisions[2]["disposition"], "rework_limit_cancelled");
    assert!(!harness.project_workspace.join(".git").exists());

    harness.shutdown().await;
}
