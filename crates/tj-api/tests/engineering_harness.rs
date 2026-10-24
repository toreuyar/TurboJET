//! TJ-037/TJ-038 integrated Engineering journey.
//!
//! This is the missing API → Manager → Flow → Stage → execution → publication
//! harness. It starts from a discovered packet and a Planning-generated task
//! plan, approves both through the real API, selects the task through the real
//! API, and then lets the production Manager run the actual Engineering stages.
//!
//! The scripted local model calls the real shell tool. Shell commands create a
//! code file and a test script in the task worktree and then execute that test.
//! The production evidence loader matches the test artifact's claimed command
//! to the durable successful `bash` tool result before the production
//! publication coordinator pushes to a disposable bare Git remote and opens a
//! draft PR on a local HTTP Forgejo-protocol fixture.
//!
//! No execution success is seeded: publication, commit and PR identity all come
//! from the Manager path. No live provider, real forge, SSH or CI is contacted.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, request::Builder};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tj_api::handlers::build_app;
use tj_manager::{ManagerHandle, spawn_manager};
use tj_test_support::{
    SyntheticPackage, neutral_capability_package_agents, neutral_capability_workflow,
};
use tower::ServiceExt;

const TEST_COMMAND: &str = "bash tests/check.sh";
const CRITERION: &str = "src/calc.sh add returns the correct sum for two numbers";

/// `TURBOJET_HOME` is process-global (it selects the per-stage output root),
/// so the two browser-like journeys in this binary run one at a time.
static HARNESS_ENV: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

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
        "id": "chatcmpl-engineering", "object": "chat.completion", "model": "scripted-model",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": text}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7}
    })
}

fn tool_response(name: &str, args: Value) -> Value {
    json!({
        "id": "chatcmpl-engineering-tool", "object": "chat.completion", "model": "scripted-model",
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

fn packet_response(source_reference: &str) -> Value {
    let mut packet: Value =
        serde_json::from_str(include_str!("../../tj-packet/fixtures/complete.json")).unwrap();
    for source in packet["sources"].as_array_mut().unwrap() {
        source["kind"] = json!("stage_artifact");
        source["reference"] = json!(source_reference);
    }
    packet["title"] = json!("Calculator add helper");
    packet["problem"]["statement"] = json!(
        "The repository has no reusable calculator helper, so repeated shell checks duplicate arithmetic logic."
    );
    packet["goals"] = json!([{
        "id": "GOAL-1",
        "statement": "Provide a reusable calculator helper with an executable test.",
        "source_refs": ["SRC-2"]
    }]);
    packet["requirements"] = json!([{
        "id": "REQ-1",
        "title": "Calculator add helper",
        "statement": "A shell helper adds two numbers and a test executes it with known values.",
        "kind": "functional",
        "priority": "must",
        "status": "confirmed",
        "acceptance_criteria": [CRITERION],
        "source_refs": ["SRC-1", "SRC-2"]
    }]);
    packet["decisions"] = json!([]);
    text_response(
        &json!({
            "verdict": "generated", "summary": "generated calculator packet",
            "content": "# Requirements packet", "packet": packet
        })
        .to_string(),
    )
}

fn plan_value() -> Value {
    json!({
        "schema_version": 1,
        "title": "Calculator helper — task plan",
        "summary": "One task adds the helper and an executable regression check.",
        "tasks": [{
            "id": "T-1",
            "title": "Add calculator helper",
            "objective": "Create src/calc.sh and a tests/check.sh that executes it.",
            "requirement_refs": ["REQ-1"],
            "decision_refs": [],
            "dependencies": [],
            "acceptance_criteria": [CRITERION],
            "components": ["src", "tests"],
            "estimate": {"size": "s", "uncertainty": "low"},
            "risks": [],
            "verification": [TEST_COMMAND]
        }],
        "scope_expansion_requests": []
    })
}

fn plan_response() -> Value {
    text_response(
        &json!({
            "verdict": "success", "summary": "traceable calculator task plan",
            "content": "# Plan", "plan": plan_value()
        })
        .to_string(),
    )
}

fn approved_response() -> Value {
    text_response(
        &json!({
            "verdict": "approved", "summary": "plan coverage complete",
            "content": "# Validation\n\nREQ-1 is covered by T-1."
        })
        .to_string(),
    )
}

fn planning_response(stage: &str) -> Value {
    match stage {
        "survey" | "outline" | "sketch" | "align" => text_response(
            &json!({
                "verdict": "success", "summary": format!("{stage} complete"),
                "content": format!("# {stage}\n\nDerived from the approved packet.")
            })
            .to_string(),
        ),
        "plan" => plan_response(),
        "check" => approved_response(),
        _ => panic!("unknown planning stage {stage}"),
    }
}

/// Follow a flow's declared entry through its positive route. No stage name is
/// assumed: the local package supplies the names and this helper only reads the
/// graph it was given.
fn flow_stage_order(flow: &Value) -> Vec<String> {
    let stages = flow["stages"]
        .as_object()
        .expect("flow has a stages object");
    let mut order = Vec::new();
    let mut current = flow["entry"].as_str().map(str::to_string);
    while let Some(stage) = current {
        if !stages.contains_key(&stage) || order.contains(&stage) {
            break;
        }
        order.push(stage.clone());
        let routes = stages[&stage]["routes"]
            .as_object()
            .expect("stage has routes");
        current = ["success", "proceed", "generated", "approved"]
            .iter()
            .find_map(|route| routes.get(*route).and_then(Value::as_str))
            .filter(|target| stages.contains_key(*target))
            .map(str::to_string);
    }
    order
}

fn stage_verdict(stage: &Value, preferred: &[&str]) -> String {
    let routes = stage["routes"].as_object().expect("stage has routes");
    preferred
        .iter()
        .find(|route| routes.contains_key(**route))
        .map(|route| (*route).to_string())
        .or_else(|| routes.keys().next().cloned())
        .expect("stage declares at least one route")
}

fn stage_response(stage: &str, verdict: &str, summary: &str) -> Value {
    text_response(
        &json!({
            "verdict": verdict, "summary": summary,
            "content": format!("# {stage}\n\nScripted local stage.")
        })
        .to_string(),
    )
}

fn implement_summary() -> Value {
    text_response(
        &json!({
            "verdict": "success", "summary": "created calculator helper and executable test",
            "content": "# Implementation\n\nCreated src/calc.sh and tests/check.sh."
        })
        .to_string(),
    )
}

fn implement_tool_command(broken: bool) -> Value {
    let operation = if broken { "-" } else { "+" };
    let command = format!(
        "mkdir -p src tests && printf '%s\\n' 'add() {{ echo $(( $1 {operation} $2 )); }}' > src/calc.sh && \
         printf '%s\\n' '#!/usr/bin/env bash' 'set -euo pipefail' \
         'source \"$(dirname \"$0\")/../src/calc.sh\"' 'value=\"$(add 20 22)\"' '[ \"$value\" = \"42\" ]' > tests/check.sh"
    );
    tool_response("bash", json!({"command": command, "timeout": 30}))
}

fn test_success() -> Value {
    text_response(
        &json!({
            "verdict": "success",
            "summary": "1 test passed",
            "content": "# Test report\n\nExecuted the generated regression check.",
            "executed_commands": [TEST_COMMAND],
            "criteria_evidence": [{
                "criterion": CRITERION,
                "evidence": "bash tests/check.sh exited 0: add 20 22 returned 42"
            }]
        })
        .to_string(),
    )
}

fn test_failure() -> Value {
    text_response(
        &json!({
            "verdict": "failed",
            "summary": "1 test failed",
            "content": "# Test report\n\nThe generated helper returned the wrong value.",
            "executed_commands": [TEST_COMMAND],
            "criteria_evidence": []
        })
        .to_string(),
    )
}

struct Forge {
    base_url: String,
    pulls: Arc<Mutex<Vec<Value>>>,
    dispatches: Arc<Mutex<Vec<Value>>>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Forge {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let pulls: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let dispatches: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let (shutdown, mut stop) = tokio::sync::oneshot::channel::<()>();
        let pulls_server = Arc::clone(&pulls);
        let dispatches_server = Arc::clone(&dispatches);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((mut stream, _)) = accepted else { continue; };
                        let pulls = Arc::clone(&pulls_server);
                        let dispatches = Arc::clone(&dispatches_server);
                        tokio::spawn(async move {
                            use tokio::io::{AsyncReadExt, AsyncWriteExt};
                            let mut buf = Vec::new();
                            let mut chunk = [0u8; 8192];
                            let header_end = loop {
                                match stream.read(&mut chunk).await {
                                    Ok(0) => return,
                                    Ok(n) => {
                                        buf.extend_from_slice(&chunk[..n]);
                                        if let Some(index) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                                            break index + 4;
                                        }
                                    }
                                    Err(_) => return,
                                }
                            };
                            let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
                            let request_line = headers.lines().next().unwrap_or_default();
                            let path = request_line.split_whitespace().nth(1).unwrap_or_default();
                            let method = request_line.split_whitespace().next().unwrap_or_default();
                            let content_length = headers.lines()
                                .find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:")
                                    .and_then(|value| value.trim().parse::<usize>().ok()))
                                .unwrap_or(0);
                            let mut body = buf[header_end..].to_vec();
                            while body.len() < content_length {
                                match stream.read(&mut chunk).await {
                                    Ok(0) => break,
                                    Ok(n) => body.extend_from_slice(&chunk[..n]),
                                    Err(_) => return,
                                }
                            }
                            let (status, payload) = if method == "GET"
                                && path.starts_with("/api/v1/repos/owner/repo/pulls")
                            {
                                (200, serde_json::to_vec(&*pulls.lock().unwrap()).unwrap())
                            } else if method == "POST" && path == "/api/v1/repos/owner/repo/pulls" {
                                let input: Value = serde_json::from_slice(&body).unwrap_or_default();
                                let branch = input.get("head").and_then(Value::as_str).unwrap_or("main");
                                let mut pulls = pulls.lock().unwrap();
                                let number = pulls.len() as u64 + 1;
                                let pull = json!({
                                    "number": number,
                                    "html_url": format!("http://localhost/pr/{number}"),
                                    "state": "open", "merged": false,
                                    "draft": input.get("draft").cloned().unwrap_or(json!(true)),
                                    "head": {"ref": branch, "label": format!("owner:{branch}")},
                                    "body": input.get("body").cloned().unwrap_or(Value::Null),
                                });
                                pulls.push(pull.clone());
                                (201, serde_json::to_vec(&pull).unwrap())
                            } else if method == "POST"
                                && path.starts_with("/api/v1/repos/owner/repo/actions/workflows/")
                            {
                                dispatches.lock().unwrap().push(
                                    serde_json::from_slice(&body).unwrap_or(Value::Null),
                                );
                                (
                                    201,
                                    serde_json::to_vec(&json!({
                                        "id": 77,
                                        "html_url": "http://localhost/actions/77"
                                    }))
                                    .unwrap(),
                                )
                            } else {
                                (404, br#"{"message":"not found"}"#.to_vec())
                            };
                            let response = format!(
                                "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                if status == 201 { "Created" } else if status == 200 { "OK" } else { "Not Found" },
                                payload.len()
                            );
                            let _ = stream.write_all(response.as_bytes()).await;
                            let _ = stream.write_all(&payload).await;
                        });
                    }
                    _ = &mut stop => break,
                }
            }
        });
        Self {
            base_url: format!("http://127.0.0.1:{port}"),
            pulls,
            dispatches,
            shutdown: Some(shutdown),
        }
    }

    fn pull_count(&self) -> usize {
        self.pulls.lock().unwrap().len()
    }
}

impl Drop for Forge {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

fn git(dir: Option<&Path>, args: &[&str]) -> String {
    let mut command = Command::new("git");
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    let output = command
        .args(args)
        .env("GIT_AUTHOR_NAME", "Harness")
        .env("GIT_AUTHOR_EMAIL", "harness@example.invalid")
        .env("GIT_COMMITTER_NAME", "Harness")
        .env("GIT_COMMITTER_EMAIL", "harness@example.invalid")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn remote_sha(remote: &Path, branch: &str) -> Option<String> {
    let output = Command::new("git")
        .arg("--git-dir")
        .arg(remote)
        .args(["rev-parse", "--verify", &format!("refs/heads/{branch}")])
        .output()
        .unwrap();
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

struct Harness {
    _temp: TempDir,
    _env_guard: tokio::sync::MutexGuard<'static, ()>,
    manager: ManagerHandle,
    app: axum::Router,
    token: String,
    project_id: String,
    lane_id: String,
    remote: PathBuf,
    _model: ScriptedModel,
    _forge: Forge,
}

impl Harness {
    async fn start() -> Self {
        Self::start_with_package(None).await
    }

    /// Run the same API → Manager → Flow → Stage → publication journey against
    /// either the synthetic SDLC-shaped package fixture or an explicit derived
    /// local SDLC package. The external package is used only by the opt-in
    /// regression; it is never read by the default suite.
    async fn start_with_package(external_package: Option<&Path>) -> Self {
        let env_guard = HARNESS_ENV.lock().await;
        let model = ScriptedModel::start().await;
        let forge = Forge::start().await;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        // Keep per-stage output directories inside this disposable harness,
        // not a shared /tmp/turbojet tree.
        // Safety: serialized by HARNESS_ENV for this test binary.
        unsafe {
            std::env::set_var("TURBOJET_HOME", root);
        }
        let config_dir = root.join("config");
        let package_dir = config_dir.join("package");
        let workflow = match external_package {
            Some(path) => {
                copy_tree(path, &package_dir);
                let manifest = package_dir.join("workflow.smg.json");
                let raw = std::fs::read_to_string(&manifest).unwrap_or_else(|error| {
                    panic!("read SDLC package {}: {error}", manifest.display())
                });
                let value: Value = serde_json::from_str(&raw).unwrap_or_else(|error| {
                    panic!("parse SDLC package {}: {error}", manifest.display())
                });
                let catalog = value["catalog"]["id"].as_str().unwrap_or_default();
                assert_eq!(
                    catalog, "full-sdlc",
                    "TJ_SDLC_PACKAGE must be the derived full-sdlc capability package"
                );
                assert!(
                    value["capabilities"]["requirements_packet"].is_object(),
                    "TJ_SDLC_PACKAGE must declare the SDLC capabilities"
                );
                value
            }
            None => {
                let workflow = neutral_capability_workflow("synthetic-engineering-harness", true);
                SyntheticPackage::at(
                    &package_dir,
                    &workflow,
                    &neutral_capability_package_agents(true),
                );
                workflow
            }
        };
        let catalog_id = workflow["catalog"]["id"]
            .as_str()
            .expect("workflow has a catalog id")
            .to_string();
        std::fs::write(package_dir.join("workflow.smg.json"), workflow.to_string()).unwrap();
        let key_env = format!("TJ_ENGINEERING_KEY_{}", uuid::Uuid::new_v4().simple());
        let delivery_key_env = format!(
            "TJ_ENGINEERING_DELIVERY_KEY_{}",
            uuid::Uuid::new_v4().simple()
        );
        let delivery_secret_env = format!(
            "TJ_ENGINEERING_DELIVERY_SECRET_{}",
            uuid::Uuid::new_v4().simple()
        );
        // Safety: unique names per harness; set before any worker reads them.
        unsafe {
            std::env::set_var(&key_env, "unused-test-key");
            std::env::set_var(&delivery_key_env, "local-delivery-token");
            std::env::set_var(&delivery_secret_env, "local-delivery-callback-secret");
            // Every harness process uses the same disposable master key so the
            // project secret can be written and resolved through the real API.
            std::env::set_var("TURBOJET_MASTER_KEY", "22".repeat(32));
        }
        std::fs::write(
            config_dir.join("turbojet.json"),
            json!({
                "listen_addr": "127.0.0.1:0", "log_level": "warn",
                "database_path": root.join("state.db").to_string_lossy(),
                "workspace_base": root.join("workspaces").to_string_lossy(),
                "workflow_sources": [{"id": "harness-workflow", "path": "config/package", "kind": "package"}],
                "providers": [{"id": "scripted", "type": "open_ai", "api_key_env": key_env,
                    "base_url": model.base_url(), "default_model": "scripted-model"}],
                "agents": [{"id": "chief", "provider_id": "scripted", "model": "scripted-model",
                    "default": true, "enabled": true, "tools": [], "system_prompt": "test"}],
                "budgets": {"question_rounds_per_stage": 6, "question_rounds_per_run": 12,
                    "model_calls": 80, "tool_calls": 40, "output_corrections": 8,
                    "interaction_nudges": 6, "active_secs": 600},
                "run_timeout_secs": 180,
                "delivery": {
                    "provider": "forgejo_actions",
                    "base_url": forge.base_url,
                    "token_env": delivery_key_env,
                    "owner": "owner", "repo": "repo",
                    "workflow": "delivery.yml", "git_ref": "main",
                    "callback_secret_env": delivery_secret_env,
                    "callback_window_secs": 300,
                    "callback_base_url": "http://turbojet.local"
                }
            })
            .to_string(),
        )
        .unwrap();

        // Disposable bare remote plus a seed commit on main.
        let remote = root.join("remote.git");
        git(None, &["init", "--bare", remote.to_str().unwrap()]);
        let seed = root.join("seed");
        std::fs::create_dir_all(seed.join("src")).unwrap();
        git(Some(&seed), &["init", "-b", "main"]);
        std::fs::write(seed.join(".gitignore"), ".turbojet/\n").unwrap();
        std::fs::write(seed.join("src/keep.txt"), "base\n").unwrap();
        git(Some(&seed), &["add", "-A"]);
        git(Some(&seed), &["commit", "-m", "base"]);
        git(
            Some(&seed),
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(Some(&seed), &["push", "-u", "origin", "main"]);

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
            Some(json!({
                "name": "Engineering integration",
                "catalog_id": catalog_id,
                "repo_url": remote.to_string_lossy(),
                "base_branch": "main",
                "git_provider": {
                    "provider_type": "forgejo",
                    "base_url": forge.base_url,
                    "owner": "owner",
                    "repo": "repo",
                    "api_token_key": "forge_token"
                }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "project: {project}");
        let project_id = project["project"]["id"].as_str().unwrap().to_string();
        let provisioned = manager.reprovision_project(&project_id).await.unwrap();
        assert_eq!(
            provisioned.provisioning_state.as_deref(),
            Some("ready"),
            "project provisioning: {provisioned:?}"
        );
        let lane_id = project["lane"]["id"].as_str().unwrap().to_string();
        let (status, secret) = call(
            &app,
            Method::POST,
            &format!("/v1/projects/{project_id}/secrets"),
            Some(&token),
            Some(json!({"key": "forge_token", "value": "local-forge-token"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "secret: {secret}");

        Self {
            _temp: temp,
            _env_guard: env_guard,
            manager,
            app,
            token,
            project_id,
            lane_id,
            remote,
            _model: model,
            _forge: forge,
        }
    }

    fn model(&self) -> &ScriptedModel {
        &self._model
    }

    fn forge(&self) -> &Forge {
        &self._forge
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
        for _ in 0..500 {
            let tree = self.tree(run_id).await;
            if predicate(&tree) {
                return tree;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let tree = self.tree(run_id).await;
        let events = self.events(run_id).await;
        let logs = self.stage_logs(run_id).await;
        let mut generated = Vec::new();
        collect_named(self._temp.path(), &mut generated);
        panic!(
            "run {run_id} did not reach expected state; generated={generated:?}; events={events}; stage_logs={logs}; tree={tree}"
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

    async fn select_task(&self, run_id: &str, body: Value) -> (StatusCode, Value) {
        call(
            &self.app,
            Method::POST,
            &format!("/v1/pipeline/runs/{run_id}/plan/select-task"),
            Some(&self.token),
            Some(body),
        )
        .await
    }

    async fn publication(&self, run_id: &str) -> Value {
        let (status, body) = call(
            &self.app,
            Method::GET,
            &format!("/v1/pipeline/runs/{run_id}/publication"),
            Some(&self.token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "publication: {body}");
        body
    }

    async fn artifacts(&self, run_id: &str) -> Value {
        let (status, body) = call(
            &self.app,
            Method::GET,
            &format!("/v1/pipeline/runs/{run_id}/artifacts"),
            Some(&self.token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "artifacts: {body}");
        body
    }

    async fn stage_logs(&self, run_id: &str) -> Value {
        let (status, body) = call(
            &self.app,
            Method::GET,
            &format!("/v1/pipeline/runs/{run_id}/stage-logs?limit=2000"),
            Some(&self.token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "stage logs: {body}");
        body
    }

    async fn events(&self, run_id: &str) -> Value {
        let (status, body) = call(
            &self.app,
            Method::GET,
            &format!("/v1/pipeline/runs/{run_id}/events"),
            Some(&self.token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "events: {body}");
        body
    }

    async fn shutdown(self) {
        let _ = self.manager.shutdown().await;
    }
}

fn collect_named(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == ".git") {
                continue;
            }
            collect_named(&path, out);
        } else if path
            .file_name()
            .is_some_and(|name| name == "calc.sh" || name == "check.sh")
        {
            out.push(path);
        }
    }
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let target = to.join(entry.file_name());
        if path.is_dir() {
            copy_tree(&path, &target);
        } else {
            std::fs::copy(&path, &target).unwrap();
        }
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

fn approve_packet(tree: &Value, packet: &Value) -> Value {
    json!({
        "approved": true, "gate_instance_id": tree["run"]["gate_instance_id"],
        "expected_state_version": tree["run"]["state_version"], "idempotency_key": "approve-packet",
        "packet_revision_id": packet["latest"]["id"], "packet_content_hash": packet["latest"]["content_hash"]
    })
}

async fn push_collect_and_shape(harness: &Harness, run: &str) -> (Value, Value, Value) {
    let model = harness.model();
    model.push(packet_response("artifact:collect:collector"));
    model.push(packet_response("artifact:collect:collector"));
    let tree = harness
        .wait_for(run, |tree| {
            tree["run"]["status"] == "waiting_for_gate"
                && tree["run"]["gate_name"] == "collect-gate"
        })
        .await;
    let packet = harness.packet(run).await;
    assert_eq!(packet["latest"]["completeness"], "complete");
    for stage in ["survey", "outline", "sketch", "align", "plan", "check"] {
        model.push(planning_response(stage));
    }
    let (status, receipt) = harness.approve(run, approve_packet(&tree, &packet)).await;
    assert_eq!(status, StatusCode::OK, "packet approval: {receipt}");
    let tree = harness
        .wait_for(run, |tree| {
            tree["run"]["status"] == "waiting_for_gate" && tree["run"]["gate_name"] == "shape-gate"
        })
        .await;
    let plan = harness.plan(run).await;
    (tree, packet, plan)
}

fn task_selection(plan: &Value) -> Value {
    json!({
        "plan_revision_id": plan["latest"]["id"],
        "plan_content_hash": plan["latest"]["content_hash"],
        "task_id": "T-1"
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manager_runs_real_shell_tests_then_publishes_verified_draft_pr_and_dispatch() {
    let harness = Harness::start().await;
    let run = harness.create_run("Engineering end-to-end").await;
    let (tree, packet, plan) = push_collect_and_shape(&harness, &run).await;
    assert_eq!(plan["latest"]["validation"]["valid"], true);
    let packet_revision_id = packet["latest"]["id"].as_str().unwrap().to_string();
    let packet_hash = packet["latest"]["content_hash"]
        .as_str()
        .unwrap()
        .to_string();
    let plan_revision_id = plan["latest"]["id"].as_str().unwrap().to_string();
    let plan_hash = plan["latest"]["content_hash"].as_str().unwrap().to_string();

    // Approving the exact plan opens the selection wait, not Engineering.
    let approved = json!({
        "approved": true, "gate_instance_id": tree["run"]["gate_instance_id"],
        "expected_state_version": tree["run"]["state_version"],
        "idempotency_key": "exact-plan-approval",
        "packet_revision_id": packet_revision_id, "packet_content_hash": packet_hash,
        "plan_revision_id": plan_revision_id, "plan_content_hash": plan_hash
    });
    let (status, receipt) = harness.approve(&run, approved).await;
    assert_eq!(status, StatusCode::OK, "plan approval: {receipt}");
    let waiting = harness
        .wait_for(&run, |tree| {
            tree["run"]["status"] == "waiting_for_gate"
                && tree["run"]["gate_state"] == "task_selection_required"
        })
        .await;
    assert!(
        !waiting["flows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|flow| flow["flow_name"] == "build"),
        "plan approval must not start Engineering without an exact selection"
    );
    assert_eq!(harness.model().request_count(), 8);
    assert_eq!(harness.publication(&run).await["publication"], Value::Null);

    // A stale/forged selection cannot start the flow; the run stays waiting.
    let forged = json!({
        "plan_revision_id": plan_revision_id,
        "plan_content_hash": "f".repeat(64),
        "task_id": "T-1"
    });
    let (status, body) = harness.select_task(&run, forged).await;
    assert_eq!(status, StatusCode::CONFLICT, "stale selection: {body}");
    let still_waiting = harness.tree(&run).await;
    assert_eq!(
        still_waiting["run"]["gate_state"],
        "task_selection_required"
    );
    assert!(
        !still_waiting["flows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|flow| flow["flow_name"] == "build")
    );

    // Real API selection; this is the only thing that releases Engineering.
    let (status, selected) = harness.select_task(&run, task_selection(&plan)).await;
    assert_eq!(status, StatusCode::CREATED, "selection: {selected}");
    assert_eq!(selected["payload"]["task"]["id"], "T-1");
    assert_eq!(selected["payload"]["packet"]["content_hash"], packet_hash);

    // Engineering: analysis, design, implementation writes real files, test
    // executes the generated script, review and acceptance.
    let model = harness.model();
    model.push(stage_response("scope", "success", "analysis complete"));
    model.push(stage_response("blueprint", "success", "design complete"));
    model.push(implement_tool_command(false));
    model.push(implement_summary());
    model.push(tool_response(
        "bash",
        json!({"command": TEST_COMMAND, "timeout": 30}),
    ));
    model.push(test_success());
    model.push(stage_response("review", "success", "review complete"));
    model.push(stage_response("accept", "success", "acceptance complete"));

    let final_tree = harness
        .wait_for(&run, |tree| tree["run"]["status"] == "completed")
        .await;
    assert_eq!(harness.model().request_count(), 16);
    assert_eq!(final_tree["run"]["status"], "completed");
    assert!(final_tree["run"]["pr_url"].is_string());
    assert_eq!(harness.forge().pull_count(), 1);

    // Production loader + coordinator produced a durable publication with the
    // exact plan/packet/task/artifact provenance.
    let publication_body = harness.publication(&run).await;
    let publication = &publication_body["publication"];
    assert_eq!(publication["task_id"], "T-1");
    assert_eq!(publication["selection_id"], selected["selection"]["id"]);
    assert_eq!(publication["plan_revision_id"], plan_revision_id);
    assert_eq!(publication["plan_content_hash"], plan_hash);
    assert_eq!(publication["packet_revision_id"], packet_revision_id);
    assert_eq!(publication["packet_content_hash"], packet_hash);
    assert_eq!(publication["push_state"], "succeeded");
    assert_eq!(publication["pr_state"], "succeeded");
    assert_eq!(publication["draft"], true);
    assert!(publication["pr_number"].as_u64().unwrap() >= 1);
    assert!(publication["pr_url"].as_str().unwrap().contains("/pr/1"));
    let commit = publication["commit_sha"].as_str().unwrap().to_string();
    assert_eq!(publication["remote_sha"].as_str(), Some(commit.as_str()));
    let branch = publication["branch"].as_str().unwrap().to_string();
    assert_eq!(
        remote_sha(&harness.remote, &branch).as_deref(),
        Some(commit.as_str()),
        "the disposable bare remote must contain the verified commit"
    );

    // The test artifact names the actual command and maps the exact approved
    // criterion; the run only reached publication because the durable bash
    // exit-0 result matched it in the production evidence loader.
    let artifacts = harness.artifacts(&run).await;
    let test_artifact = artifacts
        .as_array()
        .unwrap()
        .iter()
        .find(|artifact| artifact["stage_name"] == "test")
        .expect("test artifact");
    assert_eq!(test_artifact["id"], publication["test_artifact_id"]);
    assert_eq!(
        test_artifact["content_hash"],
        publication["test_artifact_hash"]
    );
    let test_content: Value =
        serde_json::from_str(test_artifact["content"].as_str().unwrap()).unwrap();
    assert_eq!(test_content["executed_commands"], json!([TEST_COMMAND]));
    assert_eq!(test_content["criteria_evidence"][0]["criterion"], CRITERION);
    assert_eq!(
        test_content["criteria_evidence"][0]["evidence"],
        "bash tests/check.sh exited 0: add 20 22 returned 42"
    );

    // The draft PR carries task, plan/packet and artifact provenance and is
    // never marked ready/merged.
    let pull = harness.forge().pulls.lock().unwrap()[0].clone();
    assert_eq!(pull["draft"], true);
    assert_eq!(pull["head"]["ref"], branch);
    let body = pull["body"].as_str().unwrap();
    let test_artifact_id = publication["test_artifact_id"].as_str().unwrap();
    for expected in [
        plan_revision_id.as_str(),
        plan_hash.as_str(),
        packet_revision_id.as_str(),
        packet_hash.as_str(),
        test_artifact_id,
        CRITERION,
        "src/calc.sh",
        "tests/check.sh",
        "A human reviewer owns review and merge",
    ] {
        assert!(
            body.contains(expected),
            "PR body missing '{expected}': {body}"
        );
    }

    let events = harness.events(&run).await;
    let summaries = events
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|event| event["kind"].as_str())
        .collect::<Vec<_>>();
    assert!(summaries.contains(&"draft_pr_published"), "{events}");
    assert!(
        summaries.contains(&"delivery_external_required"),
        "{events}"
    );

    // Chain an explicit staging delivery request onto the *actual* publication.
    // The local Forgejo-protocol fixture accepts the dispatch; no live CI or
    // deployment is claimed.
    let (status, delivery) = call(
        &harness.app,
        Method::POST,
        &format!("/v1/pipeline/runs/{run}/delivery/releases"),
        Some(&harness.token),
        Some(json!({
            "environment": "staging",
            "publication_id": publication["id"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "delivery dispatch: {delivery}");
    let release = &delivery["release"];
    assert_eq!(release["publication_id"], publication["id"]);
    assert_eq!(release["task_id"], "T-1");
    assert_eq!(release["plan_revision_id"], plan_revision_id);
    assert_eq!(release["packet_revision_id"], packet_revision_id);
    assert_eq!(release["commit_sha"], commit);
    assert_eq!(release["pr_number"], publication["pr_number"]);
    assert_eq!(release["expected_revision"], commit);
    assert_eq!(delivery["dispatched"], true);
    assert_eq!(harness.forge().dispatches.lock().unwrap().len(), 1);

    // The verified remote commit contains the actual artifacts created and
    // exercised by the shell tool, not just model prose.
    let calc = git(
        None,
        &[
            "--git-dir",
            harness.remote.to_str().unwrap(),
            "show",
            &format!("{commit}:src/calc.sh"),
        ],
    );
    assert_eq!(calc.trim(), "add() { echo $(( $1 + $2 )); }");
    let check = git(
        None,
        &[
            "--git-dir",
            harness.remote.to_str().unwrap(),
            "show",
            &format!("{commit}:tests/check.sh"),
        ],
    );
    assert!(check.contains("add 20 22"));
    assert!(check.contains("[ \"$value\" = \"42\" ]"));

    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failing_shell_test_stops_before_any_push_or_pr() {
    let harness = Harness::start().await;
    let run = harness.create_run("Engineering failing test").await;
    let (tree, packet, plan) = push_collect_and_shape(&harness, &run).await;
    let approved = json!({
        "approved": true, "gate_instance_id": tree["run"]["gate_instance_id"],
        "expected_state_version": tree["run"]["state_version"],
        "idempotency_key": "failing-test-plan-approval",
        "packet_revision_id": packet["latest"]["id"], "packet_content_hash": packet["latest"]["content_hash"],
        "plan_revision_id": plan["latest"]["id"], "plan_content_hash": plan["latest"]["content_hash"]
    });
    let (status, receipt) = harness.approve(&run, approved).await;
    assert_eq!(status, StatusCode::OK, "plan approval: {receipt}");
    harness
        .wait_for(&run, |tree| {
            tree["run"]["status"] == "waiting_for_gate"
                && tree["run"]["gate_state"] == "task_selection_required"
        })
        .await;
    let (status, selected) = harness.select_task(&run, task_selection(&plan)).await;
    assert_eq!(status, StatusCode::CREATED, "selection: {selected}");
    let branch = format!("tj/{}-", run);

    let model = harness.model();
    model.push(stage_response("scope", "success", "analysis complete"));
    model.push(stage_response("blueprint", "success", "design complete"));
    model.push(implement_tool_command(true));
    model.push(implement_summary());
    // The test really runs the generated failing check (non-zero exit).
    model.push(tool_response(
        "bash",
        json!({"command": TEST_COMMAND, "timeout": 30}),
    ));
    model.push(test_failure());
    // test: failed routes back to implement, which reports it cannot proceed.
    model.push(stage_response(
        "implement",
        "cannot-complete",
        "implementation cannot satisfy the failing test",
    ));

    let failed = harness
        .wait_for(&run, |tree| tree["run"]["status"] == "failed")
        .await;
    assert_eq!(failed["run"]["status"], "failed");
    assert_eq!(harness.publication(&run).await["publication"], Value::Null);
    assert_eq!(harness.forge().pull_count(), 0);

    // No branch was pushed, including under the worktree's branch prefix.
    let output = Command::new("git")
        .arg("--git-dir")
        .arg(&harness.remote)
        .args(["for-each-ref", "--format=%(refname)", "refs/heads"])
        .output()
        .unwrap();
    let refs = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        !refs
            .lines()
            .any(|reference| reference.starts_with("refs/heads/tj/") || reference.contains(&branch)),
        "failing tests must not push a task branch: {refs}"
    );
    assert_eq!(harness.model().request_count(), 15);

    harness.shutdown().await;
}

/// Opt-in proof that the actual ignored/derived local full-SDLC graph executes
/// through the same production path using only its declared capabilities. The
/// test returns SKIP when the local asset is absent; the explicit script
/// (`scripts/check-sdlc-regression.sh`) refuses to report success without it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_derived_sdlc_package_runs_declared_capabilities_end_to_end() {
    let package_path = std::env::var_os("TJ_SDLC_PACKAGE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/local/full-sdlc.package")
        });
    if !package_path.join("workflow.smg.json").is_file() {
        eprintln!(
            "SKIP local SDLC regression: {} is absent (set TJ_SDLC_PACKAGE to opt in)",
            package_path.display()
        );
        return;
    }
    let original =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/local/full-sdlc.smg.json");
    if !original.is_file() {
        eprintln!(
            "SKIP local SDLC regression: original graph {} is absent",
            original.display()
        );
        return;
    }

    let package_workflow: Value = serde_json::from_str(
        &std::fs::read_to_string(package_path.join("workflow.smg.json"))
            .expect("read local package manifest"),
    )
    .expect("parse local package manifest");
    let capabilities = &package_workflow["capabilities"];
    let packet_flow = capabilities["requirements_packet"]["produce_flow"]
        .as_str()
        .expect("package declares a packet-producing flow")
        .to_string();
    let packet_producer = capabilities["requirements_packet"]["producer_stage"]
        .as_str()
        .expect("package declares a packet producer stage")
        .to_string();
    let packet_gate = capabilities["requirements_packet"]["binding_gate"]
        .as_str()
        .expect("package declares a packet gate")
        .to_string();
    let plan_flow = capabilities["task_plan"]["produce_flow"]
        .as_str()
        .expect("package declares a plan-producing flow")
        .to_string();
    let plan_validate = capabilities["task_plan"]["validate_stage"]
        .as_str()
        .expect("package declares a plan validation stage")
        .to_string();
    let plan_gate = capabilities["task_plan"]["binding_gate"]
        .as_str()
        .expect("package declares a plan gate")
        .to_string();
    let publication_flow = capabilities["publication"]["flow"]
        .as_str()
        .expect("package declares a publication flow")
        .to_string();
    let evidence_stage = |role: &str| {
        capabilities["publication"]["required_evidence"]
            .as_array()
            .expect("package declares publication evidence")
            .iter()
            .find(|entry| entry["role"] == role)
            .and_then(|entry| entry["stage"].as_str())
            .unwrap_or_else(|| panic!("package declares no {role} evidence stage"))
            .to_string()
    };
    let test_stage = evidence_stage("test");
    let review_stage = evidence_stage("review");
    let acceptance_stage = evidence_stage("acceptance");

    let harness = Harness::start_with_package(Some(&package_path)).await;
    let (status, workflows) = call(
        &harness.app,
        Method::GET,
        "/v1/workflows",
        Some(&harness.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "workflows: {workflows}");
    assert_eq!(workflows["workflows"][0]["source_kind"], "package");
    let run = harness.create_run("Local derived SDLC end-to-end").await;
    let model = harness.model();

    // Packet flow: interactive stages complete only through the verdict tool and
    // write their declared `output.md`; the producer stage emits the packet.
    let packet_stages = flow_stage_order(&package_workflow["flows"][&packet_flow]);
    for stage in &packet_stages {
        let definition = &package_workflow["flows"][&packet_flow]["stages"][stage];
        if stage == &packet_producer {
            model.push(packet_response(&format!(
                "artifact:{packet_flow}:{packet_producer}"
            )));
        } else if definition["interactive"].as_bool() == Some(true) {
            let verdict = stage_verdict(definition, &["success", "proceed", "generated"]);
            let content = format!("# {stage}\n\nScripted local interactive stage.");
            model.push(tool_response(
                "write_file",
                json!({
                    "path": format!(".turbojet/{stage}/{run}/output.md"),
                    "content": content
                }),
            ));
            model.push(tool_response(
                "verdict",
                json!({
                    "verdict": verdict,
                    "summary": format!("{stage} complete"),
                    "content": content
                }),
            ));
        } else {
            let verdict = stage_verdict(definition, &["generated", "success", "proceed"]);
            model.push(stage_response(
                stage,
                &verdict,
                &format!("{stage} complete"),
            ));
        }
    }
    let tree = harness
        .wait_for(&run, |tree| {
            tree["run"]["status"] == "waiting_for_gate" && tree["run"]["gate_name"] == packet_gate
        })
        .await;
    let packet = harness.packet(&run).await;
    assert_eq!(packet["latest"]["completeness"], "complete");

    // Plan flow: the stage that routes into the declared validator produces the
    // plan; the validator returns the route key its own schema declares.
    let plan_stages = flow_stage_order(&package_workflow["flows"][&plan_flow]);
    let plan_producer = plan_stages
        .iter()
        .find(|stage| {
            package_workflow["flows"][&plan_flow]["stages"][*stage]["routes"]["success"].as_str()
                == Some(plan_validate.as_str())
        })
        .cloned();
    for stage in &plan_stages {
        if stage == &plan_validate {
            model.push(approved_response());
        } else if Some(stage) == plan_producer.as_ref() {
            model.push(plan_response());
        } else {
            let definition = &package_workflow["flows"][&plan_flow]["stages"][stage];
            let verdict = stage_verdict(definition, &["success", "proceed"]);
            model.push(stage_response(
                stage,
                &verdict,
                &format!("{stage} complete"),
            ));
        }
    }
    let (status, receipt) = harness.approve(&run, approve_packet(&tree, &packet)).await;
    assert_eq!(status, StatusCode::OK, "packet approval: {receipt}");
    let tree = harness
        .wait_for(&run, |tree| {
            tree["run"]["status"] == "waiting_for_gate" && tree["run"]["gate_name"] == plan_gate
        })
        .await;
    let plan = harness.plan(&run).await;
    let packet_revision_id = packet["latest"]["id"].as_str().unwrap().to_string();
    let packet_hash = packet["latest"]["content_hash"]
        .as_str()
        .unwrap()
        .to_string();
    let plan_revision_id = plan["latest"]["id"].as_str().unwrap().to_string();
    let plan_hash = plan["latest"]["content_hash"].as_str().unwrap().to_string();

    let approved = json!({
        "approved": true, "gate_instance_id": tree["run"]["gate_instance_id"],
        "expected_state_version": tree["run"]["state_version"],
        "idempotency_key": "sdlc-exact-plan-approval",
        "packet_revision_id": packet_revision_id, "packet_content_hash": packet_hash,
        "plan_revision_id": plan_revision_id, "plan_content_hash": plan_hash
    });
    let (status, receipt) = harness.approve(&run, approved).await;
    assert_eq!(status, StatusCode::OK, "plan approval: {receipt}");
    harness
        .wait_for(&run, |tree| {
            tree["run"]["status"] == "waiting_for_gate"
                && tree["run"]["gate_state"] == "task_selection_required"
        })
        .await;
    assert_eq!(harness.model().request_count(), 11);
    assert_eq!(harness.publication(&run).await["publication"], Value::Null);

    let (status, selected) = harness.select_task(&run, task_selection(&plan)).await;
    assert_eq!(status, StatusCode::CREATED, "selection: {selected}");

    // Publication flow: the stage that routes into the declared test stage writes
    // and exercises the shell script; the remaining stages use their own routes.
    let publication_stages = flow_stage_order(&package_workflow["flows"][&publication_flow]);
    let implement_stage = publication_stages
        .iter()
        .find(|stage| {
            package_workflow["flows"][&publication_flow]["stages"][*stage]["routes"]["success"]
                .as_str()
                == Some(test_stage.as_str())
        })
        .cloned();
    for stage in &publication_stages {
        if stage == &test_stage {
            model.push(tool_response(
                "bash",
                json!({"command": TEST_COMMAND, "timeout": 30}),
            ));
            model.push(test_success());
        } else if Some(stage) == implement_stage.as_ref() {
            model.push(implement_tool_command(false));
            model.push(implement_summary());
        } else if stage == &review_stage {
            model.push(stage_response(stage, "success", "review complete"));
        } else if stage == &acceptance_stage {
            model.push(stage_response(stage, "success", "acceptance complete"));
        } else {
            model.push(stage_response(stage, "success", "analysis complete"));
        }
    }
    let final_tree = harness
        .wait_for(&run, |tree| tree["run"]["status"] == "completed")
        .await;
    assert_eq!(final_tree["run"]["status"], "completed");
    assert!(final_tree["run"]["pr_url"].is_string());
    assert_eq!(harness.forge().pull_count(), 1);

    let publication_body = harness.publication(&run).await;
    let publication = &publication_body["publication"];
    assert_eq!(publication["task_id"], "T-1");
    assert_eq!(publication["selection_id"], selected["selection"]["id"]);
    assert_eq!(publication["plan_revision_id"], plan_revision_id);
    assert_eq!(publication["plan_content_hash"], plan_hash);
    assert_eq!(publication["packet_revision_id"], packet_revision_id);
    assert_eq!(publication["packet_content_hash"], packet_hash);
    assert_eq!(publication["push_state"], "succeeded");
    assert_eq!(publication["pr_state"], "succeeded");
    assert_eq!(publication["draft"], true);
    let commit = publication["commit_sha"].as_str().unwrap().to_string();
    assert_eq!(publication["remote_sha"].as_str(), Some(commit.as_str()));
    let branch = publication["branch"].as_str().unwrap().to_string();
    assert_eq!(
        remote_sha(&harness.remote, &branch).as_deref(),
        Some(commit.as_str())
    );

    let artifacts = harness.artifacts(&run).await;
    let test_artifact = artifacts
        .as_array()
        .unwrap()
        .iter()
        .find(|artifact| artifact["stage_name"] == test_stage.as_str())
        .expect("test artifact");
    assert_eq!(test_artifact["id"], publication["test_artifact_id"]);
    assert_eq!(
        test_artifact["content_hash"],
        publication["test_artifact_hash"]
    );

    let events = harness.events(&run).await;
    let kinds = events
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|event| event["kind"].as_str())
        .collect::<Vec<_>>();
    assert!(kinds.contains(&"draft_pr_published"), "{events}");
    assert!(kinds.contains(&"delivery_external_required"), "{events}");

    let (status, delivery) = call(
        &harness.app,
        Method::POST,
        &format!("/v1/pipeline/runs/{run}/delivery/releases"),
        Some(&harness.token),
        Some(json!({
            "environment": "staging",
            "publication_id": publication["id"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "delivery dispatch: {delivery}");
    assert_eq!(delivery["release"]["publication_id"], publication["id"]);
    assert_eq!(delivery["release"]["task_id"], "T-1");
    assert_eq!(delivery["dispatched"], true);
    assert_eq!(harness.forge().dispatches.lock().unwrap().len(), 1);

    harness.shutdown().await;
}
