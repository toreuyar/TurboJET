//! Batch 1 workflow-package integration: import a synthetic package, freeze its
//! assets at admission, and keep admitting runs after the source directory is
//! edited, deleted and the process restarts.
//!
//! Everything here is synthetic and disposable. No shipped workflow, provider,
//! repository or external service is used.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, request::Builder};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tj_api::handlers::build_app;
use tj_config::ConfigLoader;
use tj_manager::{ManagerHandle, spawn_manager};
use tower::ServiceExt;

struct ScriptedModel {
    port: u16,
    responses: Arc<Mutex<VecDeque<Value>>>,
    _requests: Arc<AtomicUsize>,
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
            _requests: requests,
            shutdown: Some(shutdown),
        }
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn push(&self, response: Value) {
        self.responses.lock().unwrap().push_back(response);
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
        "id": "chatcmpl-package",
        "object": "chat.completion",
        "model": "scripted-model",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": text}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7}
    })
}

fn verdict(value: &str) -> Value {
    json!({
        "id": "chatcmpl-package-tool",
        "object": "chat.completion",
        "model": "scripted-model",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{"id": "call-verdict", "type": "function",
                    "function": {"name": "verdict",
                        "arguments": json!({"verdict": value, "content": "package result", "summary": "scripted"}).to_string()}}]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7}
    })
}

struct Harness {
    manager: ManagerHandle,
    app: axum::Router,
    token: String,
    project_id: String,
    lane_id: String,
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

const CONFIG_SOURCE: &str = "sources/package";
const ORIGINAL_PROMPT: &str = "Original package prompt.\n";

fn write(path: &std::path::Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn write_config(root: &std::path::Path, model: &ScriptedModel, sources: Value) {
    let key_env = format!("TJ_PACKAGE_KEY_{}", uuid::Uuid::new_v4().simple());
    // Safety: unique test-only key set before any worker starts.
    unsafe {
        std::env::set_var(&key_env, "unused-local-key");
    }
    write(
        &root.join("config/fixture.json"),
        &json!({
            "listen_addr": "127.0.0.1:0",
            "log_level": "warn",
            "database_path": root.join("state.db").to_string_lossy(),
            "workspace_base": root.join("workspaces").to_string_lossy(),
            "adc_dir": root.join("config/stages").to_string_lossy(),
            "workflow_sources": sources,
            "providers": [{"id": "scripted", "type": "open_ai", "api_key_env": key_env,
                "base_url": model.base_url(), "default_model": "scripted-model"}],
            "agents": [{"id": "chief", "provider_id": "scripted", "model": "scripted-model",
                "default": true, "enabled": true, "tools": [], "system_prompt": "fixture"}],
            "budgets": {
                "question_rounds_per_stage": 6, "question_rounds_per_run": 12,
                "model_calls": 20, "tool_calls": 20, "output_corrections": 4,
                "interaction_nudges": 4, "active_secs": 120
            },
            "run_timeout_secs": 60
        })
        .to_string(),
    );
}

fn write_package(root: &std::path::Path) {
    let package = root.join(CONFIG_SOURCE);
    let graph = json!({
        "version": 1,
        "name": "Synthetic package harness",
        "catalog": {"id": "package-harness"},
        "maturity": "experimental",
        "description": "Disposable package integration fixture",
        "entry": "work",
        "exit": "work",
        "limits": {"maxIterations": 2, "maxSteps": 6, "maxDepth": 3},
        "flow-order": ["work"],
        "flows": {
            "work": {
                "type": "flow",
                "entry": "step",
                "exit": "step",
                "stages": {
                    "step": {"agent": "step", "interactive": true, "routes": {"success": "_exit"}}
                }
            }
        }
    });
    write(&package.join("workflow.smg.json"), &graph.to_string());
    write(
        &package.join("agents/step/agent.json"),
        &json!({
            "id": "step", "name": "Step", "prompt": "prompt.md",
            "schema": "schemas/out.json", "timeout": 20, "max_output_retries": 3,
            "tools": {"read": false, "write": false, "edit": false, "bash": false}
        })
        .to_string(),
    );
    write(&package.join("agents/step/prompt.md"), ORIGINAL_PROMPT);
    write(
        &package.join("schemas/out.json"),
        &json!({
            "type": "object",
            "required": ["verdict"],
            "properties": {"verdict": {"type": "string", "enum": ["success"]}}
        })
        .to_string(),
    );
}

async fn start(root: &std::path::Path, register: bool, token: Option<String>) -> Harness {
    let loader = ConfigLoader::from_file(&root.join("config/fixture.json")).unwrap();
    let spawn = spawn_manager(loader).await.unwrap();
    let manager = spawn.handle.clone();
    let app = build_app(spawn).await.unwrap();
    let mut token = token.unwrap_or_default();
    if register {
        let (status, auth) = call(
            &app,
            Method::POST,
            "/v1/auth/register",
            None,
            Some(json!({"username": "admin", "password": "package-pass"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "register: {auth}");
        token = auth["token"].as_str().unwrap().to_string();
        let (status, created) = call(
            &app,
            Method::POST,
            "/v1/projects",
            Some(&token),
            Some(json!({"name": "Package project", "catalog_id": "package-harness"})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "project: {created}");
        let project_id = created["project"]["id"].as_str().unwrap().to_string();
        let lane_id = created["lane"]["id"].as_str().unwrap().to_string();
        return Harness {
            manager,
            app,
            token,
            project_id,
            lane_id,
        };
    }
    // The token/user/project/lane rows are already durable from the first run.
    let (status, workflows) = call(&app, Method::GET, "/v1/workflows", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "workflows: {workflows}");
    let workflow = &workflows["workflows"][0];
    let (status, projects) = call(&app, Method::GET, "/v1/projects", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "projects: {projects}");
    let project = &projects.as_array().unwrap()[0];
    let project_id = project["id"].as_str().unwrap().to_string();
    let lane = &project["lanes"].as_array().unwrap()[0];
    assert_eq!(lane["workflow_id"], workflow["id"]);
    Harness {
        manager,
        app,
        token,
        project_id,
        lane_id: lane["id"].as_str().unwrap().to_string(),
    }
}

impl Harness {
    async fn create_run(&self, model: &ScriptedModel, description: &str) -> String {
        model.push(verdict("success"));
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

    async fn wait_completed(&self, run_id: &str) -> Value {
        for _ in 0..200 {
            let (status, tree) = call(
                &self.app,
                Method::GET,
                &format!("/v1/pipeline/runs/{run_id}/tree"),
                Some(&self.token),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "tree: {tree}");
            if tree["run"]["status"] == "completed" {
                return tree;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("run {run_id} did not complete");
    }

    async fn bundle(&self, run_id: &str) -> Value {
        let (status, body) = call(
            &self.app,
            Method::GET,
            &format!("/v1/pipeline/runs/{run_id}/bundle"),
            Some(&self.token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "bundle: {body}");
        body
    }
}

#[tokio::test]
async fn package_runs_use_frozen_registry_assets_after_source_edit_delete_and_restart() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let model = ScriptedModel::start().await;
    write_package(root);
    write_config(
        root,
        &model,
        json!([{"id": "package-source", "path": CONFIG_SOURCE, "kind": "package"}]),
    );

    let first = start(root, true, None).await;
    let token = first.token.clone();

    let (status, workflows) =
        call(&first.app, Method::GET, "/v1/workflows", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "workflows: {workflows}");
    assert_eq!(workflows["workflows"].as_array().unwrap().len(), 1);
    assert_eq!(workflows["workflows"][0]["source_kind"], "package");
    assert_eq!(workflows["workflows"][0]["asset_count"], 4);

    // First run through the real scripted model path.
    let run_one = first.create_run(&model, "package run one").await;
    first.wait_completed(&run_one).await;
    let bundle_one = first.bundle(&run_one).await;
    let stage_one = &bundle_one["bundle"]["flows"]["work"]["step"];
    assert_eq!(stage_one["prompt"], ORIGINAL_PROMPT);
    assert_eq!(stage_one["prompt_path"], "agents/step/prompt.md");
    assert_eq!(stage_one["adc_path"], "agents/step/agent.json");
    assert_eq!(stage_one["schema_path"], "schemas/out.json");

    // Edit the prompt/schema and delete the whole source package. Admission
    // must still use the stored assets and freeze exactly the imported bytes.
    write(
        &root.join(CONFIG_SOURCE).join("agents/step/prompt.md"),
        "Edited prompt that must never be frozen.\n",
    );
    std::fs::remove_file(root.join(CONFIG_SOURCE).join("schemas/out.json")).unwrap();
    std::fs::remove_file(root.join(CONFIG_SOURCE).join("agents/step/agent.json")).unwrap();

    let run_two = first
        .create_run(&model, "package run after source edit")
        .await;
    first.wait_completed(&run_two).await;
    let bundle_two = first.bundle(&run_two).await;
    let stage_two = &bundle_two["bundle"]["flows"]["work"]["step"];
    assert_eq!(stage_two["prompt"], ORIGINAL_PROMPT);
    assert_eq!(
        stage_two["prompt_hash"], stage_one["prompt_hash"],
        "admitted-run behavior must not change when source files change"
    );
    assert_eq!(stage_two["adc_path"], "agents/step/agent.json");
    assert_eq!(stage_two["schema_path"], "schemas/out.json");

    // Remove the package directory completely and restart with no configured
    // sources. The registry remains the source of truth and admits a new run.
    std::fs::remove_dir_all(root.join(CONFIG_SOURCE)).unwrap();
    let _ = first.manager.shutdown().await;
    write_config(root, &model, json!([]));

    let restarted = start(root, false, Some(token)).await;
    let (status, workflows) = call(
        &restarted.app,
        Method::GET,
        "/v1/workflows",
        Some(&restarted.token),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "workflows after restart: {workflows}"
    );
    assert_eq!(workflows["workflows"].as_array().unwrap().len(), 1);
    assert_eq!(workflows["workflows"][0]["source_kind"], "package");
    assert_eq!(workflows["workflows"][0]["asset_count"], 4);
    assert!(workflows["configured"].is_null());

    let run_three = restarted
        .create_run(&model, "package run after restart")
        .await;
    restarted.wait_completed(&run_three).await;
    let bundle_three = restarted.bundle(&run_three).await;
    assert_eq!(
        bundle_three["bundle"]["flows"]["work"]["step"]["prompt"],
        ORIGINAL_PROMPT
    );
    let _ = restarted.manager.shutdown().await;
}

#[tokio::test]
async fn directory_discovered_package_survives_package_directory_removal_and_restart() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let model = ScriptedModel::start().await;
    write_package(root);
    write_config(
        root,
        &model,
        json!([{"id": "mixed-source", "path": "sources", "kind": "directory"}]),
    );

    let first = start(root, true, None).await;
    let token = first.token.clone();
    let (status, workflows) =
        call(&first.app, Method::GET, "/v1/workflows", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "workflows: {workflows}");
    assert_eq!(workflows["workflows"][0]["source_kind"], "package");
    let run_one = first.create_run(&model, "directory package run").await;
    first.wait_completed(&run_one).await;

    // The configured directory still exists; only the imported package
    // subdirectory is removed. Restart must not fail and admission must read
    // the registry assets.
    std::fs::remove_dir_all(root.join(CONFIG_SOURCE)).unwrap();
    let _ = first.manager.shutdown().await;
    let restarted = start(root, false, Some(token)).await;
    let run_two = restarted.create_run(&model, "registry package run").await;
    restarted.wait_completed(&run_two).await;
    let bundle = restarted.bundle(&run_two).await;
    assert_eq!(
        bundle["bundle"]["flows"]["work"]["step"]["prompt"],
        ORIGINAL_PROMPT
    );
    let _ = restarted.manager.shutdown().await;
}

#[tokio::test]
async fn package_import_rejection_is_actionable_and_leaves_the_registry_unchanged() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let model = ScriptedModel::start().await;
    write_package(root);
    write_config(
        root,
        &model,
        json!([{"id": "package-source", "path": CONFIG_SOURCE, "kind": "package"}]),
    );
    let harness = start(root, true, None).await;

    // Make the configured package invalid after startup, then re-import.
    write(
        &root.join(CONFIG_SOURCE).join("agents/step/agent.json"),
        "{not json",
    );
    let (status, body) = call(
        &harness.app,
        Method::POST,
        "/v1/workflows/import",
        Some(&harness.token),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "import: {body}");
    let error = body["error"].as_str().unwrap();
    assert!(error.contains("agents/step/agent.json"), "{error}");
    assert!(error.contains("invalid ADC"), "{error}");

    let (status, workflows) = call(
        &harness.app,
        Method::GET,
        "/v1/workflows",
        Some(&harness.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(workflows["workflows"].as_array().unwrap().len(), 1);
    assert_eq!(workflows["workflows"][0]["asset_count"], 4);
    let _ = harness.manager.shutdown().await;
}
