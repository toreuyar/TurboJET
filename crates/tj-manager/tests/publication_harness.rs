//! TJ-038 local disposable-forge scenarios.
//!
//! These tests use a real disposable bare Git repository for the push protocol
//! and a local HTTP server implementing the Forgejo PR endpoints for the PR
//! protocol. They prove the local coordinator/reconciliation behaviour only.
//! A real Forgejo/SSH pilot remains externally BLOCKED.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tj_db::plans::PlanRevision;
use tj_db::publications::{NewPublication, PublicationState};
use tj_db::{Database, RunArtifact};
use tj_git::{GitProviderConfig, providers::forgejo::ForgejoProvider};
use tj_manager::publication::{
    EngineeringEvidence, PublishRequest, publication_operation_key, publish_draft_pr,
};

struct Forge {
    base_url: String,
    pulls: Arc<Mutex<Vec<Value>>>,
    fail_next_pr: Arc<AtomicBool>,
    lose_next_pr_response: Arc<AtomicBool>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Forge {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let pulls: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let fail_next_pr = Arc::new(AtomicBool::new(false));
        let lose_next_pr_response = Arc::new(AtomicBool::new(false));
        let (shutdown, mut stop) = tokio::sync::oneshot::channel::<()>();
        let pulls_for_server = Arc::clone(&pulls);
        let fail = Arc::clone(&fail_next_pr);
        let lose = Arc::clone(&lose_next_pr_response);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((mut stream, _)) = accepted else { continue; };
                        let pulls = Arc::clone(&pulls_for_server);
                        let fail = Arc::clone(&fail);
                        let lose = Arc::clone(&lose);
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
                            let mut lines = headers.lines();
                            let request_line = lines.next().unwrap_or_default();
                            let mut parts = request_line.split_whitespace();
                            let method = parts.next().unwrap_or_default();
                            let path = parts.next().unwrap_or_default();
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
                            if method == "GET" && path.starts_with("/api/v1/repos/owner/repo/pulls") {
                                let body = serde_json::to_vec(&*pulls.lock().unwrap()).unwrap();
                                let response = format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                    body.len()
                                );
                                let _ = stream.write_all(response.as_bytes()).await;
                                let _ = stream.write_all(&body).await;
                                return;
                            }
                            if method == "POST" && path == "/api/v1/repos/owner/repo/pulls" {
                                if fail.swap(false, Ordering::SeqCst) {
                                    let body = br#"{"message":"scripted forge failure"}"#;
                                    let response = format!(
                                        "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                        body.len()
                                    );
                                    let _ = stream.write_all(response.as_bytes()).await;
                                    let _ = stream.write_all(body).await;
                                    return;
                                }
                                let input: Value = serde_json::from_slice(&body).unwrap_or_default();
                                let branch = input.get("head").and_then(Value::as_str).unwrap_or("main").to_string();
                                let pull = {
                                    let mut pulls = pulls.lock().unwrap();
                                    let number = pulls.len() as u64 + 1;
                                    let pull = json!({
                                        "number": number,
                                        "html_url": format!("http://localhost/pr/{number}"),
                                        "state": "open",
                                        "merged": false,
                                        "draft": input.get("draft").cloned().unwrap_or(json!(true)),
                                        "head": {"ref": branch, "label": format!("owner:{branch}")},
                                        "body": input.get("body").cloned().unwrap_or(Value::Null),
                                    });
                                    pulls.push(pull.clone());
                                    pull
                                };
                                if lose.swap(false, Ordering::SeqCst) {
                                    // Simulate a lost HTTP response after the
                                    // forge committed the PR.
                                    drop(stream);
                                    return;
                                }
                                let body = serde_json::to_vec(&pull).unwrap();
                                let response = format!(
                                    "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                    body.len()
                                );
                                let _ = stream.write_all(response.as_bytes()).await;
                                let _ = stream.write_all(&body).await;
                                return;
                            }
                            let body = br#"{"message":"not found"}"#;
                            let response = format!(
                                "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                            let _ = stream.write_all(response.as_bytes()).await;
                            let _ = stream.write_all(body).await;
                        });
                    }
                    _ = &mut stop => break,
                }
            }
        });
        Self {
            base_url: format!("http://127.0.0.1:{port}"),
            pulls,
            fail_next_pr,
            lose_next_pr_response,
            shutdown: Some(shutdown),
        }
    }

    fn provider(&self) -> ForgejoProvider {
        ForgejoProvider::from_config_and_token(
            &GitProviderConfig {
                provider_type: "forgejo".into(),
                base_url: self.base_url.clone(),
                owner: "owner".into(),
                repo: "repo".into(),
                api_token_key: "unused".into(),
                transport_key: None,
            },
            "test-token".into(),
        )
        .unwrap()
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

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
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

struct Fixture {
    _temp: tempfile::TempDir,
    db: Arc<Database>,
    remote: PathBuf,
    worktree: PathBuf,
    base_sha: String,
    branch: String,
    evidence: EngineeringEvidence,
}

fn fixture(reject_push: bool, test_verdict: &str) -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let db = Arc::new(Database::open(root.join("db.sqlite").to_str().unwrap()).unwrap());
    // Durable shell activity written by the tool boundary, not by the model.
    // The publication gate matches the test artifact's executed command to
    // this successful exit-0 result.
    db.append_stage_log(&tj_db::StageLog {
        id: 0,
        run_id: tj_db::RunId("run".into()),
        stage_id: "stage_test".into(),
        stage_name: Some("test".into()),
        flow_name: Some("engineering".into()),
        kind: "tool_call".into(),
        content: r#"bash {"command":"cargo test","timeout":10}"#.into(),
        created_at: "2026-01-01T00:00:00Z".into(),
    })
    .unwrap();
    db.append_stage_log(&tj_db::StageLog {
        id: 0,
        run_id: tj_db::RunId("run".into()),
        stage_id: "stage_test".into(),
        stage_name: Some("test".into()),
        flow_name: Some("engineering".into()),
        kind: "tool_result".into(),
        content: r#"bash → {"success":true,"stdout":"test result: ok","stderr":"","exit_code":0}"#
            .into(),
        created_at: "2026-01-01T00:00:00Z".into(),
    })
    .unwrap();
    let seed = root.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "-b", "main"]);
    std::fs::create_dir_all(seed.join("src")).unwrap();
    std::fs::write(seed.join("src/lib.rs"), "pub fn base() {}\n").unwrap();
    git(&seed, &["add", "-A"]);
    git(&seed, &["commit", "-m", "base"]);

    let remote = root.join("remote.git");
    let output = Command::new("git")
        .args(["init", "--bare", remote.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success());
    git(
        &seed,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&seed, &["push", "-u", "origin", "main"]);
    if reject_push {
        let hook = remote.join("hooks/pre-receive");
        std::fs::write(
            &hook,
            "#!/bin/sh\necho 'scripted forge rejects push' >&2\nexit 1\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let branch = "tj/run-task".to_string();
    let worktree = root.join("worktree");
    git(
        &seed,
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            worktree.to_str().unwrap(),
            "main",
        ],
    );
    std::fs::write(
        worktree.join("src/lib.rs"),
        "pub fn base() {}\npub fn fix() {}\n",
    )
    .unwrap();
    let base_sha = git(&worktree, &["rev-parse", "main"]);

    let plan = PlanRevision {
        id: "plan_rev_1".into(),
        plan_id: "plan_1".into(),
        run_id: "run".into(),
        revision: 1,
        schema_version: 1,
        content_json: "{}".into(),
        content_hash: "a".repeat(64),
        packet_revision_id: "packet_rev_1".into(),
        packet_content_hash: "b".repeat(64),
        packet_id: "packet_1".into(),
        packet_revision: 1,
        parent_revision_id: None,
        source_artifact_id: None,
        source_artifact_hash: None,
        flow_name: Some("planning".into()),
        stage_name: Some("plan".into()),
        change_request: None,
        validation_json: "{}".into(),
        created_by: "alice".into(),
        created_at: "2026-01-01T00:00:00Z".into(),
    };
    let task = tj_plan::PlanTask {
        id: "T-1".into(),
        title: "Small bugfix".into(),
        objective: "Fix the bug".into(),
        requirement_refs: vec!["REQ-1".into()],
        enabling_rationale: None,
        decision_refs: vec![],
        dependencies: vec![],
        acceptance_criteria: vec!["the bug is fixed".into()],
        components: vec!["src".into()],
        estimate: tj_plan::TaskEstimate {
            size: "s".into(),
            uncertainty: "low".into(),
            rationale: None,
        },
        risks: vec![],
        verification: vec!["cargo test".into()],
        notes: None,
    };
    let selection = tj_db::plans::PlanTaskSelection {
        id: "selection_1".into(),
        run_id: "run".into(),
        plan_id: "plan_1".into(),
        plan_revision_id: "plan_rev_1".into(),
        plan_content_hash: "a".repeat(64),
        task_id: "T-1".into(),
        selected_by: "alice".into(),
        created_at: "2026-01-01T00:00:00Z".into(),
        payload_json: json!({
            "plan": {"content_hash": "a".repeat(64)},
            "task": {"id": "T-1", "title": "Small bugfix"},
            "readiness": {"ready": true}
        })
        .to_string(),
    };
    let artifact = |stage: &str, verdict: &str| RunArtifact {
        id: format!("art_{stage}"),
        run_id: tj_db::RunId("run".into()),
        flow_name: "engineering".into(),
        stage_name: stage.into(),
        attempt: 0,
        version: 1,
        content: if stage == "test" {
            json!({
                "verdict": verdict,
                "summary": stage,
                "executed_commands": ["cargo test"],
                "criteria_evidence": [
                    {"criterion": "the bug is fixed", "evidence": "cargo test passed"}
                ]
            })
            .to_string()
        } else {
            json!({"verdict": verdict, "summary": stage}).to_string()
        },
        content_hash: match stage {
            "test" => "c",
            "review" => "d",
            _ => "e",
        }
        .repeat(64),
        created_at: "2026-01-01T00:00:00Z".into(),
    };
    let evidence = EngineeringEvidence {
        selection: Some(selection),
        task: Some(task),
        plan: Some(plan),
        items: vec![
            tj_manager::publication::RoleEvidence {
                role: tj_config::capability::PublicationRole::Test,
                stage: "test".into(),
                artifact: artifact("test", test_verdict),
                execution: Some(tj_config::capability::EvidenceExecution::Shell),
                criterion_mapping: true,
            },
            tj_manager::publication::RoleEvidence {
                role: tj_config::capability::PublicationRole::Review,
                stage: "review".into(),
                artifact: artifact("review", "success"),
                execution: None,
                criterion_mapping: false,
            },
            tj_manager::publication::RoleEvidence {
                role: tj_config::capability::PublicationRole::Acceptance,
                stage: "acceptance".into(),
                artifact: artifact("acceptance", "success"),
                execution: None,
                criterion_mapping: false,
            },
        ],
        changed_files: vec!["src/lib.rs".into()],
        scope_violations: vec![],
    };
    Fixture {
        _temp: temp,
        db,
        remote,
        worktree,
        base_sha,
        branch,
        evidence,
    }
}

fn request<'a>(fixture: &'a Fixture, evidence: &'a EngineeringEvidence) -> PublishRequest<'a> {
    PublishRequest {
        run_id: "run",
        flow_name: "engineering",
        attempt_id: "attempt_1",
        workspace: &fixture.worktree,
        branch: &fixture.branch,
        base_branch: "main",
        base_sha: &fixture.base_sha,
        evidence,
        ssh_env: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bugfix_publishes_verified_sha_and_exactly_one_draft_pr() {
    let forge = Forge::start().await;
    let fixture = fixture(false, "success");
    let provider = forge.provider();
    let publication = publish_draft_pr(
        &fixture.db,
        Some(&provider),
        request(&fixture, &fixture.evidence),
    )
    .await
    .unwrap();
    assert_eq!(publication.push_state, PublicationState::Succeeded);
    assert_eq!(publication.pr_state, PublicationState::Succeeded);
    assert_eq!(publication.task_id, "T-1");
    assert!(publication.draft);
    let remote = remote_sha(&fixture.remote, &fixture.branch).expect("remote branch");
    assert_eq!(Some(remote), publication.remote_sha);
    assert_eq!(forge.pull_count(), 1);
    let pull = forge.pulls.lock().unwrap()[0].clone();
    assert_eq!(pull["head"]["ref"], fixture.branch);
    assert_eq!(pull["draft"], true);
    let body = pull["body"].as_str().unwrap();
    assert!(body.contains("plan_rev_1"));
    assert!(body.contains("packet_rev_1"));
    assert!(body.contains("art_test"));
    assert!(body.contains("A human reviewer owns review and merge"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn renamed_flows_and_declared_evidence_stages_publish_without_name_inference() {
    let forge = Forge::start().await;
    let mut fixture = fixture(false, "success");
    let flow = "deliver-code";
    let stages = ["compile-check", "peer-review", "sign-off"];
    for (item, stage) in fixture.evidence.items.iter_mut().zip(stages) {
        item.stage = stage.into();
        item.artifact.flow_name = flow.into();
        item.artifact.stage_name = stage.into();
    }
    // Durable shell activity for the declared test stage, not a business name.
    fixture
        .db
        .append_stage_log(&tj_db::StageLog {
            id: 0,
            run_id: tj_db::RunId("run".into()),
            stage_id: "stage_compile".into(),
            stage_name: Some("compile-check".into()),
            flow_name: Some(flow.into()),
            kind: "tool_call".into(),
            content: r#"bash {"command":"cargo test","timeout":10}"#.into(),
            created_at: "2026-01-01T00:00:01Z".into(),
        })
        .unwrap();
    fixture
        .db
        .append_stage_log(&tj_db::StageLog {
            id: 0,
            run_id: tj_db::RunId("run".into()),
            stage_id: "stage_compile".into(),
            stage_name: Some("compile-check".into()),
            flow_name: Some(flow.into()),
            kind: "tool_result".into(),
            content: r#"bash → {"success":true,"stdout":"ok","stderr":"","exit_code":0}"#.into(),
            created_at: "2026-01-01T00:00:02Z".into(),
        })
        .unwrap();
    let provider = forge.provider();
    let publication = publish_draft_pr(
        &fixture.db,
        Some(&provider),
        PublishRequest {
            run_id: "run",
            flow_name: flow,
            attempt_id: "attempt_1",
            workspace: &fixture.worktree,
            branch: &fixture.branch,
            base_branch: "main",
            base_sha: &fixture.base_sha,
            evidence: &fixture.evidence,
            ssh_env: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(publication.push_state, PublicationState::Succeeded);
    assert_eq!(publication.pr_state, PublicationState::Succeeded);
    assert_eq!(publication.flow_name, flow);
    let evidence: Value =
        serde_json::from_str(publication.evidence_json.as_deref().unwrap()).unwrap();
    assert_eq!(evidence["roles"][0]["stage"], "compile-check");
    let pull = forge.pulls.lock().unwrap()[0].clone();
    let body = pull["body"].as_str().unwrap();
    assert!(body.contains("compile-check"), "{body}");
    assert!(body.contains("peer-review"), "{body}");
    assert!(body.contains("sign-off"), "{body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_only_test_verdict_without_durable_execution_is_refused() {
    let forge = Forge::start().await;
    let mut claimed = fixture(false, "success");
    claimed.evidence.items[0].artifact.content = json!({
        "verdict": "success", "summary": "claimed, never run",
        "criteria_evidence": [
            {"criterion": "the bug is fixed", "evidence": "claimed only"}
        ]
    })
    .to_string();
    let provider = forge.provider();
    let error = publish_draft_pr(
        &claimed.db,
        Some(&provider),
        request(&claimed, &claimed.evidence),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("executed_commands"), "{error}");
    assert_eq!(forge.pull_count(), 0);
    assert!(remote_sha(&claimed.remote, &claimed.branch).is_none());

    // A command that was never executed under the shell tool is refused even
    // when it looks like a real test command.
    let mut fabricated = fixture(false, "success");
    fabricated.evidence.items[0].artifact.content = json!({
        "verdict": "success", "summary": "fabricated",
        "executed_commands": ["cargo test --all"],
        "criteria_evidence": [
            {"criterion": "the bug is fixed", "evidence": "fabricated"}
        ]
    })
    .to_string();
    let error = publish_draft_pr(
        &fabricated.db,
        Some(&provider),
        request(&fabricated, &fabricated.evidence),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("no successful durable bash execution"),
        "{error}"
    );
    assert_eq!(forge.pull_count(), 0);

    // A successful command is not enough if the task's acceptance criteria are
    // not mapped to evidence.
    let mut unmapped = fixture(false, "success");
    unmapped.evidence.items[0].artifact.content = json!({
        "verdict": "success", "summary": "command ran but no criterion mapping",
        "executed_commands": ["cargo test"], "criteria_evidence": []
    })
    .to_string();
    let error = publish_draft_pr(
        &unmapped.db,
        Some(&provider),
        request(&unmapped, &unmapped.evidence),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("acceptance criterion"), "{error}");
    assert_eq!(forge.pull_count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn feature_scope_is_checked_and_failed_test_refuses_publication() {
    let forge = Forge::start().await;
    let failed_fixture = fixture(false, "failed");
    let provider = forge.provider();
    let error = publish_draft_pr(
        &failed_fixture.db,
        Some(&provider),
        request(&failed_fixture, &failed_fixture.evidence),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("test"), "{error}");
    assert!(remote_sha(&failed_fixture.remote, &failed_fixture.branch).is_none());
    assert_eq!(forge.pull_count(), 0);
    assert!(
        failed_fixture.worktree.join("src/lib.rs").exists(),
        "workspace retained"
    );

    let mut out_of_scope = fixture(false, "success");
    out_of_scope.evidence.changed_files = vec!["unapproved/file.rs".into()];
    out_of_scope.evidence.scope_violations = vec!["unapproved/file.rs".into()];
    let error = publish_draft_pr(
        &out_of_scope.db,
        Some(&provider),
        request(&out_of_scope, &out_of_scope.evidence),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("outside the approved task scope"), "{error}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejected_push_cannot_be_recorded_as_success_and_keeps_workspace() {
    let forge = Forge::start().await;
    let fixture = fixture(true, "success");
    let provider = forge.provider();
    let error = publish_draft_pr(
        &fixture.db,
        Some(&provider),
        request(&fixture, &fixture.evidence),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("push"), "{error}");
    let publication = fixture
        .db
        .latest_publication_for_run("run")
        .unwrap()
        .unwrap();
    assert_eq!(publication.push_state, PublicationState::Failed);
    assert!(publication.push_error.unwrap().contains("rejects push"));
    assert_eq!(publication.pr_state, PublicationState::Pending);
    assert_eq!(forge.pull_count(), 0);
    assert!(
        fixture.worktree.join("src/lib.rs").exists(),
        "workspace retained"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lost_pr_response_reconciles_without_a_duplicate() {
    let forge = Forge::start().await;
    forge.lose_next_pr_response.store(true, Ordering::SeqCst);
    let fixture = fixture(false, "success");
    let provider = forge.provider();
    let publication = publish_draft_pr(
        &fixture.db,
        Some(&provider),
        request(&fixture, &fixture.evidence),
    )
    .await
    .unwrap();
    assert_eq!(publication.pr_state, PublicationState::Succeeded);
    assert_eq!(forge.pull_count(), 1);
    // Idempotent second invocation: same operation key, no duplicate PR.
    let again = publish_draft_pr(
        &fixture.db,
        Some(&provider),
        request(&fixture, &fixture.evidence),
    )
    .await
    .unwrap();
    assert_eq!(again.pr_number, publication.pr_number);
    assert_eq!(forge.pull_count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pr_failure_reports_failure_but_retains_verified_pushed_branch() {
    let forge = Forge::start().await;
    forge.fail_next_pr.store(true, Ordering::SeqCst);
    let fixture = fixture(false, "success");
    let provider = forge.provider();
    let error = publish_draft_pr(
        &fixture.db,
        Some(&provider),
        request(&fixture, &fixture.evidence),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("PR") || error.contains("status"), "{error}");
    let publication = fixture
        .db
        .latest_publication_for_run("run")
        .unwrap()
        .unwrap();
    assert_eq!(publication.push_state, PublicationState::Succeeded);
    assert!(publication.remote_sha.is_some());
    assert_eq!(publication.pr_state, PublicationState::Failed);
    assert_eq!(forge.pull_count(), 0);
    assert!(
        fixture.worktree.join("src/lib.rs").exists(),
        "workspace retained"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_reconciles_an_unknown_push_before_opening_the_pr() {
    let forge = Forge::start().await;
    let fixture = fixture(false, "success");
    let provider = forge.provider();
    // Simulate a crash after the remote accepted the push but before the
    // publication row was updated, including a durable commit identity.
    git(&fixture.worktree, &["add", "-A"]);
    git(&fixture.worktree, &["commit", "-m", "task"]);
    let commit = git(&fixture.worktree, &["rev-parse", "HEAD"]);
    let request = request(&fixture, &fixture.evidence);
    let key = publication_operation_key(&request, &commit);
    let (publication, _) = fixture
        .db
        .create_publication(&NewPublication {
            id: "pub_restart".into(),
            run_id: "run".into(),
            flow_name: "engineering".into(),
            attempt_id: "attempt_1".into(),
            operation_key: key,
            selection_id: "selection_1".into(),
            task_id: "T-1".into(),
            plan_revision_id: "plan_rev_1".into(),
            plan_content_hash: "a".repeat(64),
            packet_revision_id: "packet_rev_1".into(),
            packet_content_hash: "b".repeat(64),
            branch: fixture.branch.clone(),
            base_branch: "main".into(),
            workspace_path: fixture.worktree.display().to_string(),
            base_sha: Some(fixture.base_sha.clone()),
            commit_sha: Some(commit.clone()),
            test_artifact_id: "art_test".into(),
            test_artifact_hash: "c".repeat(64),
            review_artifact_id: "art_review".into(),
            review_artifact_hash: "d".repeat(64),
            acceptance_artifact_id: "art_acceptance".into(),
            acceptance_artifact_hash: "e".repeat(64),
            scope_files_json: r#"["src/lib.rs"]"#.into(),
            scope_violations_json: "[]".into(),
            evidence_json: None,
            draft: true,
            created_at: "2026-01-01T00:00:00Z".into(),
        })
        .unwrap();
    fixture.db.mark_push_running(&publication.id).unwrap();
    fixture
        .db
        .mark_push_unknown(&publication.id, "process restarted")
        .unwrap();
    git(
        &fixture.worktree,
        &["push", "-u", "origin", &fixture.branch],
    );

    let reconciled = publish_draft_pr(&fixture.db, Some(&provider), request)
        .await
        .unwrap();
    assert_eq!(reconciled.push_state, PublicationState::Succeeded);
    assert_eq!(reconciled.remote_sha.as_deref(), Some(commit.as_str()));
    assert_eq!(reconciled.pr_state, PublicationState::Succeeded);
    assert_eq!(forge.pull_count(), 1);
    assert_eq!(remote_sha(&fixture.remote, &fixture.branch), Some(commit));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_before_publication_leaves_no_remote_effect_and_retains_workspace() {
    let fixture = fixture(false, "success");
    // A cancelled attempt never invokes the coordinator; the local approved
    // work remains available and no branch/PR exists.
    assert!(fixture.worktree.join("src/lib.rs").exists());
    assert!(remote_sha(&fixture.remote, &fixture.branch).is_none());
    assert!(
        fixture
            .db
            .latest_publication_for_run("run")
            .unwrap()
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn evidence_loader_requires_successful_test_review_and_acceptance_artifacts() {
    let fixture = fixture(false, "failed");
    let capability =
        tj_manager::legacy_v1::LegacyV1Policy::Full.publication_capability("engineering");
    let error = tj_manager::publication::load_publication_evidence(
        &fixture.db,
        "run",
        "engineering",
        &capability,
        vec!["src/lib.rs".into()],
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("approved structured plan"), "{error}");
}
