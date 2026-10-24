//! Plan-less publication end-to-end proofs (default, asset-free).
//!
//! The main journey declares a repository workspace and a publication
//! capability that requires **two** evidence roles: real shell test evidence at
//! the `verify` stage and review evidence at the `inspect` stage. It
//! deliberately declares **no** requirements packet, task plan or task
//! selection. The run must execute through the real API -> Manager -> Flow ->
//! Stage -> SQLite path, produce its own evidence artifacts, commit/push to a
//! disposable bare Git remote, and open exactly one draft PR through a local
//! Forgejo-protocol fixture.
//!
//! Additional cases exercise the declared contract rather than an inferred
//! one: a review-only publication (the policy intentionally allows it), and
//! fail-closed negative paths for missing shell evidence, a non-zero test,
//! absent declared test evidence, and an absent/mismatched declared review
//! artifact. Each negative asserts a durable failure reason, zero forge
//! requests and no remote branch/PR.
//!
//! No success row, task identity, plan hash, packet hash, or publication
//! evidence is seeded. The only inputs are the synthetic graph/assets, a
//! scripted local model, and a fresh bare repository.

use std::collections::{BTreeMap, VecDeque};
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
        "id": "chatcmpl-planless", "object": "chat.completion", "model": "scripted-model",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": text}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7}
    })
}

fn tool_response(name: &str, args: Value) -> Value {
    json!({
        "id": "chatcmpl-planless-tool", "object": "chat.completion", "model": "scripted-model",
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

struct Forge {
    base_url: String,
    pulls: Arc<Mutex<Vec<Value>>>,
    requests: Arc<AtomicUsize>,
    pr_posts: Arc<AtomicUsize>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Forge {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let pulls: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let requests = Arc::new(AtomicUsize::new(0));
        let pr_posts = Arc::new(AtomicUsize::new(0));
        let (shutdown, mut stop) = tokio::sync::oneshot::channel::<()>();
        let pulls_server = Arc::clone(&pulls);
        let requests_server = Arc::clone(&requests);
        let pr_posts_server = Arc::clone(&pr_posts);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((mut stream, _)) = accepted else { continue; };
                        let pulls = Arc::clone(&pulls_server);
                        let requests = Arc::clone(&requests_server);
                        let pr_posts = Arc::clone(&pr_posts_server);
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
                            requests.fetch_add(1, Ordering::SeqCst);
                            if method == "POST" && path == "/api/v1/repos/owner/repo/pulls" {
                                pr_posts.fetch_add(1, Ordering::SeqCst);
                            }
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
            requests,
            pr_posts,
            shutdown: Some(shutdown),
        }
    }

    fn pull_count(&self) -> usize {
        self.pulls.lock().unwrap().len()
    }

    fn request_count(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    fn pr_post_count(&self) -> usize {
        self.pr_posts.load(Ordering::SeqCst)
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

/// Every branch ref in the disposable remote, so a negative case can prove the
/// push never happened instead of trusting a publication row that might be
/// missing for another reason.
fn remote_refs(remote: &Path) -> BTreeMap<String, String> {
    let output = Command::new("git")
        .arg("--git-dir")
        .arg(remote)
        .args([
            "for-each-ref",
            "--format=%(refname:short) %(objectname)",
            "refs/heads",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git for-each-ref failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once(' '))
        .map(|(name, sha)| (name.to_string(), sha.to_string()))
        .collect()
}

fn write_agent(package_dir: &Path, name: &str, tools: Value, verdicts: &[&str]) {
    let agent_dir = package_dir.join("agents").join(name);
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::create_dir_all(package_dir.join("schemas")).unwrap();
    std::fs::write(
        agent_dir.join("prompt.md"),
        format!("Synthetic {name} stage. Return the declared JSON contract."),
    )
    .unwrap();
    std::fs::write(
        package_dir.join("schemas").join(format!("{name}.json")),
        json!({
            "type": "object",
            "required": ["verdict", "summary"],
            "properties": {
                "verdict": {"type": "string", "enum": verdicts},
                "summary": {"type": "string"},
                "content": {"type": "string"}
            }
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        agent_dir.join("agent.json"),
        json!({
            "id": name,
            "name": name,
            "prompt": "prompt.md",
            "schema": format!("schemas/{name}.json"),
            "timeout": 20,
            "max_output_retries": 3,
            "tools": tools
        })
        .to_string(),
    )
    .unwrap();
}

fn full_evidence() -> Value {
    json!([
        {"role": "test", "stage": "verify", "execution": "shell"},
        {"role": "review", "stage": "inspect"}
    ])
}

fn standard_routes() -> Value {
    json!({
        "edit": {"success": "verify"},
        "verify": {"success": "inspect"},
        "inspect": {"success": "_exit"}
    })
}

/// Build the synthetic graph from an explicit publication declaration and
/// explicit stage routes. Extra route keys (for example `failed`) make a
/// declared evidence stage reachable for graph validation while the real
/// `success` path can still bypass it; no test asset or local SDLC file is read.
fn graph(
    _package_dir: &Path,
    required_evidence: Value,
    entry: &str,
    exit: &str,
    routes: &Value,
) -> Value {
    let agent = |name: &str| name.to_string();
    let mut stages = serde_json::Map::new();
    for (name, stage_routes) in routes.as_object().expect("stage routes must be an object") {
        stages.insert(
            name.clone(),
            json!({
                "agent": agent(name),
                "interactive": false,
                "routes": stage_routes
            }),
        );
    }
    json!({
        "version": 1,
        "name": "planless publication",
        "catalog": {"id": "planless-publication", "label": "Plan-less publication"},
        "maturity": "experimental",
        "description": "Repository publication with declared evidence and no task plan",
        "capabilities": {
            "schema_version": 1,
            "workspace": {"kind": "repository", "provisioning": "optional"},
            "publication": {
                "flow": "build",
                "mode": "draft_pr",
                "required_evidence": required_evidence
            }
        },
        "entry": "build",
        "exit": "build",
        "limits": {"maxIterations": 1, "maxSteps": 6, "maxDepth": 2},
        "flow-order": ["build"],
        "flows": {
            "build": {
                "type": "flow",
                "entry": entry,
                "exit": exit,
                "stages": stages
            }
        }
    })
}

fn call(
    app: &axum::Router,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> impl std::future::Future<Output = (StatusCode, Value)> {
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
    let app = app.clone();
    async move {
        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
}

/// `TURBOJET_HOME` and the manager key are process-global, so the journeys in
/// this binary run one at a time even though each has its own temp root.
static HARNESS_ENV: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

struct Harness {
    _temp: TempDir,
    manager: ManagerHandle,
    app: axum::Router,
    token: String,
    project_id: String,
    lane_id: String,
    remote: PathBuf,
    main_sha: String,
    _model: ScriptedModel,
    _forge: Forge,
    // Last field so it drops after the manager and env-dependent fixtures.
    _env_guard: tokio::sync::MutexGuard<'static, ()>,
}

impl Harness {
    async fn start(required_evidence: Value, entry: &str, exit: &str, routes: &Value) -> Self {
        let env_guard = HARNESS_ENV.lock().await;
        let model = ScriptedModel::start().await;
        let forge = Forge::start().await;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        unsafe {
            std::env::set_var("TURBOJET_HOME", root);
            std::env::set_var("TURBOJET_MASTER_KEY", "33".repeat(32));
        }
        let config_dir = root.join("config");
        let source_dir = config_dir.join("sources/planless-package");
        std::fs::create_dir_all(&source_dir).unwrap();
        let edit_verdicts: Vec<&str> = routes["edit"]
            .as_object()
            .expect("edit routes")
            .keys()
            .map(String::as_str)
            .collect();
        write_agent(
            &source_dir,
            "edit",
            json!({"read": true, "write": true, "edit": false, "bash": false}),
            &edit_verdicts,
        );
        write_agent(
            &source_dir,
            "verify",
            json!({"read": true, "write": false, "edit": false, "bash": true}),
            &["success"],
        );
        write_agent(
            &source_dir,
            "inspect",
            json!({"read": false, "write": false, "edit": false, "bash": false}),
            &["success"],
        );
        write_agent(
            &source_dir,
            "review",
            json!({"read": false, "write": false, "edit": false, "bash": false}),
            &["success"],
        );
        std::fs::write(
            source_dir.join("workflow.smg.json"),
            graph(&source_dir, required_evidence, entry, exit, routes).to_string(),
        )
        .unwrap();

        let key_env = format!("TJ_PLANLESS_KEY_{}", uuid::Uuid::new_v4().simple());
        unsafe {
            std::env::set_var(&key_env, "unused-test-key");
        }
        std::fs::write(
            config_dir.join("turbojet.json"),
            json!({
                "listen_addr": "127.0.0.1:0",
                "log_level": "warn",
                "database_path": root.join("state.db").to_string_lossy(),
                "workspace_base": root.join("workspaces").to_string_lossy(),
                "workflow_sources": [{
                    "id": "planless",
                    "path": "config/sources/planless-package",
                    "kind": "package"
                }],
                "providers": [{
                    "id": "scripted", "type": "open_ai", "api_key_env": key_env,
                    "base_url": model.base_url(), "default_model": "scripted-model"
                }],
                "agents": [{
                    "id": "chief", "provider_id": "scripted", "model": "scripted-model",
                    "default": true, "enabled": true, "tools": [], "system_prompt": "test"
                }],
                "budgets": {
                    "question_rounds_per_stage": 6, "question_rounds_per_run": 12,
                    "model_calls": 20, "tool_calls": 10, "output_corrections": 8,
                    "interaction_nudges": 6, "active_secs": 120
                },
                "run_timeout_secs": 120
            })
            .to_string(),
        )
        .unwrap();

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
        let main_sha = remote_sha(&remote, "main").expect("seeded remote main");

        let loader = tj_config::ConfigLoader::from_file(&config_dir.join("turbojet.json")).unwrap();
        let spawn = spawn_manager(loader).await.unwrap();
        let manager = spawn.handle.clone();
        let app = build_app(spawn).await.unwrap();
        let (status, auth) = call(
            &app,
            Method::POST,
            "/v1/auth/register",
            None,
            Some(json!({"username": "admin", "password": "planless-pass"})),
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
                "name": "Plan-less publication",
                "catalog_id": "planless-publication",
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
            main_sha,
            _model: model,
            _forge: forge,
        }
    }

    async fn create_run(&self) -> String {
        let (status, body) = call(
            &self.app,
            Method::POST,
            "/v1/pipeline/runs",
            Some(&self.token),
            Some(json!({
                "description": "plan-less publication journey",
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

    async fn events(&self, run_id: &str) -> Vec<Value> {
        let (status, body) = call(
            &self.app,
            Method::GET,
            &format!("/v1/pipeline/runs/{run_id}/events"),
            Some(&self.token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "events: {body}");
        body.as_array().cloned().unwrap_or_default()
    }

    /// The run reached publication after its real stages completed, the
    /// declared evidence contract refused it, and there is no durable
    /// publication row, forge effect or remote branch.
    async fn assert_publication_refused(&self, run_id: &str, reason: &str) {
        let tree = self.tree(run_id).await;
        assert_eq!(tree["run"]["status"], "failed", "{tree}");
        assert!(tree["run"]["pr_url"].is_null(), "{tree}");
        assert!(
            tree["flows"]
                .as_array()
                .unwrap()
                .iter()
                .all(|flow| flow["status"] == "completed"),
            "the flow itself must complete before the publication gate refuses: {tree}"
        );

        let publication_body = self.publication(run_id).await;
        assert!(
            publication_body["publication"].is_null(),
            "refused publication must not fabricate a publication row: {publication_body}"
        );
        assert_eq!(
            publication_body["history"].as_array().unwrap().len(),
            0,
            "{publication_body}"
        );
        assert_eq!(
            publication_body["draft_pr"],
            Value::Null,
            "{publication_body}"
        );

        let events = self.events(run_id).await;
        assert!(
            !events
                .iter()
                .any(|event| event["kind"] == "draft_pr_published"),
            "no publication success event may be emitted: {events:?}"
        );
        let failures: Vec<_> = events
            .iter()
            .filter(|event| event["kind"] == "run_failed")
            .collect();
        assert_eq!(failures.len(), 1, "durable failure event: {events:?}");
        let summary = failures[0]["summary"]
            .as_str()
            .expect("durable failure summary");
        assert!(
            summary.contains(reason),
            "expected failure reason containing {reason:?}, got {summary:?}"
        );

        let refs = remote_refs(&self.remote);
        assert_eq!(
            refs,
            BTreeMap::from([("main".to_string(), self.main_sha.clone())]),
            "a refused publication must leave the remote refs untouched"
        );
        assert_eq!(self._forge.request_count(), 0, "forge was contacted");
        assert_eq!(
            self._forge.pr_post_count(),
            0,
            "forge PR POST was attempted"
        );
        assert_eq!(self._forge.pull_count(), 0, "forge recorded a PR");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plan_less_publication_runs_real_stages_and_opens_one_draft_pr() {
    let harness = Harness::start(full_evidence(), "edit", "inspect", &standard_routes()).await;
    let model = &harness._model;
    // Edit stage: a real write tool creates an executable check script. The
    // script itself creates the repository change when the verify stage runs.
    model.push(tool_response(
        "write_file",
        json!({
            "path": "tests/check.sh",
            "content": "#!/usr/bin/env bash\nset -euo pipefail\nmkdir -p src\nprintf '%s\\n' 'plan-less publication' > src/feature.txt\ngrep -q 'plan-less publication' src/feature.txt\n"
        }),
    ));
    model.push(text_response(
        &json!({
            "verdict": "success",
            "summary": "created the executable check script",
            "content": "# Edit\n\nCreated tests/check.sh with the write tool."
        })
        .to_string(),
    ));
    // Test evidence stage: the real shell tool runs the script. The declared
    // policy is execution=shell with criterion_mapping omitted, so the artifact
    // must name the command and durable stage activity must prove exit 0 --
    // without inventing an approved task or acceptance criteria.
    model.push(tool_response(
        "bash",
        json!({"command": "bash tests/check.sh", "timeout": 30}),
    ));
    model.push(text_response(
        &json!({
            "verdict": "success",
            "summary": "the executed check passed",
            "content": "# Test report\n\nbash tests/check.sh exited 0.",
            "executed_commands": ["bash tests/check.sh"],
            "criteria_evidence": []
        })
        .to_string(),
    ));
    // Review evidence stage: a real stage-executed success artifact.
    model.push(text_response(
        &json!({
            "verdict": "success",
            "summary": "reviewed the change without a task plan",
            "content": "# Review\n\nThe declared review evidence is complete."
        })
        .to_string(),
    ));

    let run = harness.create_run().await;
    let tree = harness
        .wait_for(&run, |tree| tree["run"]["status"] == "completed")
        .await;
    assert_eq!(tree["run"]["status"], "completed");
    assert!(
        tree["run"]["gate_binding"].is_null(),
        "this workflow has no gate: {tree}"
    );
    assert!(tree["run"]["pr_url"].is_string());

    let publication_body = harness.publication(&run).await;
    let publication = &publication_body["publication"];
    assert_eq!(publication["push_state"], "succeeded");
    assert_eq!(publication["pr_state"], "succeeded");
    assert_eq!(publication["draft"], true);
    // A plan-less run must not invent task/plan/packet identity.
    assert_eq!(publication["task_id"], "");
    assert_eq!(publication["selection_id"], "");
    assert_eq!(publication["plan_revision_id"], "");
    assert_eq!(publication["plan_content_hash"], "");
    assert_eq!(publication["packet_revision_id"], "");
    assert_eq!(publication["packet_content_hash"], "");
    let evidence: Value =
        serde_json::from_str(publication["evidence_json"].as_str().unwrap()).unwrap();
    assert_eq!(evidence["roles"].as_array().unwrap().len(), 2);
    assert_eq!(evidence["roles"][0]["role"], "test");
    assert_eq!(evidence["roles"][0]["stage"], "verify");
    assert_eq!(evidence["roles"][1]["role"], "review");
    assert_eq!(evidence["roles"][1]["stage"], "inspect");

    let artifacts = harness.artifacts(&run).await;
    let test_artifact = artifacts
        .as_array()
        .unwrap()
        .iter()
        .find(|artifact| artifact["stage_name"] == "verify")
        .expect("verify artifact");
    assert_eq!(
        test_artifact["content_hash"],
        evidence["roles"][0]["content_hash"]
    );
    let inspect = artifacts
        .as_array()
        .unwrap()
        .iter()
        .find(|artifact| artifact["stage_name"] == "inspect")
        .expect("inspect artifact");
    assert_eq!(
        inspect["content_hash"],
        evidence["roles"][1]["content_hash"]
    );

    let commit = publication["commit_sha"].as_str().unwrap();
    let branch = publication["branch"].as_str().unwrap();
    assert_eq!(publication["remote_sha"].as_str(), Some(commit));
    assert_eq!(
        remote_sha(&harness.remote, branch).as_deref(),
        Some(commit),
        "the disposable remote must contain the verified commit"
    );
    assert_eq!(harness._forge.pull_count(), 1);
    assert_eq!(model.request_count(), 5);

    let _ = harness.manager.shutdown().await;
}

const PASSING_CHECK_SCRIPT: &str = "#!/usr/bin/env bash\nset -euo pipefail\nmkdir -p src\nprintf '%s\\n' 'plan-less publication' > src/feature.txt\ngrep -q 'plan-less publication' src/feature.txt\n";
const FAILING_CHECK_SCRIPT: &str = "#!/usr/bin/env bash\nexit 7\n";

fn edit_writes_check(model: &ScriptedModel, script: &str) {
    model.push(tool_response(
        "write_file",
        json!({"path": "tests/check.sh", "content": script}),
    ));
    model.push(text_response(
        &json!({
            "verdict": "success",
            "summary": "created the test script",
            "content": "# Edit\n\nCreated tests/check.sh with the write tool."
        })
        .to_string(),
    ));
}

fn verify_runs_check(model: &ScriptedModel) {
    model.push(tool_response(
        "bash",
        json!({"command": "bash tests/check.sh", "timeout": 30}),
    ));
    model.push(text_response(
        &json!({
            "verdict": "success",
            "summary": "the check command was executed",
            "content": "# Test report\n\nbash tests/check.sh was executed.",
            "executed_commands": ["bash tests/check.sh"],
            "criteria_evidence": []
        })
        .to_string(),
    ));
}

fn review_success(model: &ScriptedModel) {
    model.push(text_response(
        &json!({
            "verdict": "success",
            "summary": "reviewed the change without a task plan",
            "content": "# Review\n\nThe declared review evidence is complete."
        })
        .to_string(),
    ));
}

async fn wait_failed(harness: &Harness, run: &str) {
    harness
        .wait_for(run, |tree| tree["run"]["status"] == "failed")
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn review_only_publication_is_explicit_and_does_not_invent_test_identity() {
    // The policy deliberately accepts a review-only plan-less declaration; this
    // positive case proves it works without adding an undeclared test stage.
    let evidence = json!([{"role": "review", "stage": "inspect"}]);
    let routes = json!({
        "edit": {"success": "inspect"},
        "inspect": {"success": "_exit"}
    });
    let harness = Harness::start(evidence, "edit", "inspect", &routes).await;
    let model = &harness._model;
    model.push(tool_response(
        "write_file",
        json!({"path": "src/reviewed.txt", "content": "review-only publication\n"}),
    ));
    model.push(text_response(
        &json!({
            "verdict": "success",
            "summary": "created the reviewed change",
            "content": "# Edit\n\nCreated src/reviewed.txt."
        })
        .to_string(),
    ));
    review_success(model);

    let run = harness.create_run().await;
    let tree = harness
        .wait_for(&run, |tree| tree["run"]["status"] == "completed")
        .await;
    assert_eq!(tree["run"]["status"], "completed", "{tree}");
    assert!(tree["run"]["pr_url"].is_string(), "{tree}");

    let publication_body = harness.publication(&run).await;
    let publication = &publication_body["publication"];
    assert_eq!(publication["push_state"], "succeeded");
    assert_eq!(publication["pr_state"], "succeeded");
    assert_eq!(publication["draft"], true);
    assert_eq!(publication["task_id"], "");
    assert_eq!(publication["selection_id"], "");
    assert_eq!(publication["plan_revision_id"], "");
    assert_eq!(publication["packet_revision_id"], "");
    let evidence: Value =
        serde_json::from_str(publication["evidence_json"].as_str().unwrap()).unwrap();
    assert_eq!(evidence["roles"].as_array().unwrap().len(), 1);
    assert_eq!(evidence["roles"][0]["role"], "review");
    assert_eq!(evidence["roles"][0]["stage"], "inspect");

    let artifacts = harness.artifacts(&run).await;
    let stages: Vec<_> = artifacts
        .as_array()
        .unwrap()
        .iter()
        .map(|artifact| artifact["stage_name"].as_str().unwrap().to_string())
        .collect();
    assert!(stages.contains(&"inspect".to_string()), "{stages:?}");
    assert!(
        !stages.contains(&"verify".to_string()),
        "review-only publication must not invent a test stage: {stages:?}"
    );

    let commit = publication["commit_sha"].as_str().unwrap();
    let branch = publication["branch"].as_str().unwrap();
    assert_eq!(publication["remote_sha"].as_str(), Some(commit));
    assert_eq!(remote_sha(&harness.remote, branch).as_deref(), Some(commit));
    assert_eq!(harness._forge.pull_count(), 1);
    assert_eq!(model.request_count(), 3);

    let _ = harness.manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn missing_shell_command_evidence_fails_closed_without_remote_effect() {
    let harness = Harness::start(full_evidence(), "edit", "inspect", &standard_routes()).await;
    let model = &harness._model;
    edit_writes_check(model, PASSING_CHECK_SCRIPT);
    // A model verdict that never names a durable shell command is not evidence.
    model.push(text_response(
        &json!({
            "verdict": "success",
            "summary": "claimed the check passed",
            "content": "# Test report\n\nNo command was recorded."
        })
        .to_string(),
    ));
    review_success(model);

    let run = harness.create_run().await;
    wait_failed(&harness, &run).await;
    harness
        .assert_publication_refused(&run, "does not list executed_commands")
        .await;
    assert_eq!(model.request_count(), 4);

    let _ = harness.manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nonzero_executed_test_refuses_publication_despite_success_verdict() {
    let harness = Harness::start(full_evidence(), "edit", "inspect", &standard_routes()).await;
    let model = &harness._model;
    edit_writes_check(model, FAILING_CHECK_SCRIPT);
    // The model still claims success; the durable shell result exits non-zero.
    verify_runs_check(model);
    review_success(model);

    let run = harness.create_run().await;
    wait_failed(&harness, &run).await;
    harness
        .assert_publication_refused(&run, "ran a test/build/check command that failed")
        .await;
    assert_eq!(model.request_count(), 5);

    let _ = harness.manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn absent_declared_test_evidence_fails_closed_before_any_remote_effect() {
    // `verify` exists in the graph (so graph validation passes) and is reachable
    // through a non-success route, but the real success path never executes it.
    let routes = json!({
        "edit": {"success": "inspect", "failed": "verify"},
        "verify": {"success": "inspect"},
        "inspect": {"success": "_exit"}
    });
    let harness = Harness::start(full_evidence(), "edit", "inspect", &routes).await;
    let model = &harness._model;
    edit_writes_check(model, PASSING_CHECK_SCRIPT);
    review_success(model);

    let run = harness.create_run().await;
    wait_failed(&harness, &run).await;
    harness
        .assert_publication_refused(
            &run,
            "did not produce required 'Test' evidence at stage 'verify'",
        )
        .await;
    assert_eq!(model.request_count(), 3);

    let _ = harness.manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn absent_declared_review_artifact_fails_closed_after_passing_test_evidence() {
    // `inspect` is declared and reachable, but the success path exits at the
    // verified test stage, so no review artifact exists at publication time.
    let routes = json!({
        "edit": {"success": "verify", "failed": "inspect"},
        "verify": {"success": "_exit"},
        "inspect": {"success": "verify"}
    });
    let harness = Harness::start(full_evidence(), "edit", "verify", &routes).await;
    let model = &harness._model;
    edit_writes_check(model, PASSING_CHECK_SCRIPT);
    verify_runs_check(model);

    let run = harness.create_run().await;
    wait_failed(&harness, &run).await;
    harness
        .assert_publication_refused(
            &run,
            "did not produce required 'Review' evidence at stage 'inspect'",
        )
        .await;
    assert_eq!(model.request_count(), 4);

    let _ = harness.manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mismatched_declared_review_stage_is_not_satisfied_by_another_review_artifact() {
    // A real, successful review artifact exists at stage `review`, but the
    // declared contract names stage `inspect`. Publication must not fall back to
    // a different stage by role or business name.
    let routes = json!({
        "edit": {"success": "verify", "failed": "inspect"},
        "verify": {"success": "review"},
        "review": {"success": "_exit"},
        "inspect": {"success": "review"}
    });
    let harness = Harness::start(full_evidence(), "edit", "review", &routes).await;
    let model = &harness._model;
    edit_writes_check(model, PASSING_CHECK_SCRIPT);
    verify_runs_check(model);
    review_success(model); // runs at stage `review`, not the declared `inspect`

    let run = harness.create_run().await;
    wait_failed(&harness, &run).await;
    harness
        .assert_publication_refused(
            &run,
            "did not produce required 'Review' evidence at stage 'inspect'",
        )
        .await;

    let artifacts = harness.artifacts(&run).await;
    let staged_review = artifacts
        .as_array()
        .unwrap()
        .iter()
        .find(|artifact| artifact["stage_name"] == "review")
        .expect("the success path produced a review artifact at the wrong stage");
    let content: Value = serde_json::from_str(staged_review["content"].as_str().unwrap()).unwrap();
    assert_eq!(content["verdict"], "success");
    assert!(
        artifacts
            .as_array()
            .unwrap()
            .iter()
            .all(|artifact| artifact["stage_name"] != "inspect"),
        "no artifact exists at the declared stage: {artifacts}"
    );
    assert_eq!(model.request_count(), 5);

    let _ = harness.manager.shutdown().await;
}
