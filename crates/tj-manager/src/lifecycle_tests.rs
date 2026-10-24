use super::*;

fn actor() -> (ManagerActor, PathBuf) {
    let dir = std::env::temp_dir().join(format!("tj-manager-lifecycle-{}", uuid::Uuid::new_v4()));
    let db = Arc::new(Database::open(&dir.join("state.db").to_string_lossy()).unwrap());
    db.save_run(&tj_db::PipelineRun::new(
        "run".into(),
        None,
        None,
        "test".into(),
        RunStatus::Running,
        None,
        None,
        None,
        None,
    ))
    .unwrap();
    let (tx, _) = mpsc::unbounded_channel();
    let (pipeline_events, _) = broadcast::channel(16);
    let tree = tj_test_support::SyntheticTree::at(dir.join("fixture"));
    let loader = Arc::new(ConfigLoader::from_file(tree.config_path()).unwrap());
    let graph = loader.load_workflow_sources().unwrap().remove(0).graph;
    let bundle = tj_config::bundle::ExecutionBundle::build(&loader, "run", None, &graph).unwrap();
    let bundle_record = tj_db::ExecutionBundleRecord {
        id: "bundle_run".into(),
        run_id: tj_db::RunId("run".into()),
        workflow_id: None,
        bundle_hash: bundle.bundle_hash.clone(),
        bundle_json: serde_json::to_string(&bundle).unwrap(),
        legacy: false,
        created_at: bundle.created_at.clone(),
    };
    db.save_execution_bundle(&bundle_record).unwrap();
    let mut run = db.load_run("run").unwrap().unwrap();
    run.set_execution_bundle(
        Some(bundle_record.id.clone()),
        Some(bundle_record.bundle_hash.clone()),
    );
    db.save_run(&run).unwrap();
    (
        ManagerActor {
            db,
            smg_cache: std::sync::Mutex::new(HashMap::new()),
            contract_cache: std::sync::Mutex::new(HashMap::new()),
            legacy_policy_cache: std::sync::Mutex::new(HashMap::new()),
            config_loader: loader,
            tx,
            watchdogs: HashMap::new(),
            executions: HashMap::new(),
            cleanup_blocked: HashSet::new(),
            pipeline_events,
            chat: None,
            worktrees: HashMap::new(),
            worktree_manager: None,
            git_providers: HashMap::new(),
            secrets_store: None,
            notifier: None,
            git_provider_warned: HashSet::new(),
            workspace_base: dir.to_string_lossy().into_owned(),
            web_bridges: Arc::new(DashMap::new()),
        },
        dir,
    )
}

fn actor_graph(actor: &ManagerActor) -> StateMachineGraph {
    actor
        .config_loader
        .load_workflow_sources()
        .expect("synthetic workflow source")
        .into_iter()
        .next()
        .expect("one synthetic workflow")
        .graph
}

fn completion(attempt_id: &str) -> ManagerEvent {
    ManagerEvent::FlowDone {
        attempt_id: attempt_id.into(),
        run_id: "run".into(),
        flow_name: "flow-a".into(),
        trace_id: "test".into(),
        outcome: FlowOutcome::Failed {
            flow_id: "flow".into(),
            run_id: "run".into(),
            flow_name: "flow-a".into(),
            error: "scripted failure".into(),
        },
    }
}

#[tokio::test]
async fn chief_operations_are_user_scoped_and_revalidate_sessions() {
    let (actor, dir) = actor();
    let db = actor.db.clone();
    for id in ["admin", "alice", "bob"] {
        db.create_user(&tj_db::User {
            id: id.into(),
            username: id.into(),
            password_hash: "unused".into(),
            display_name: None,
            is_admin: id == "admin",
            created_at: String::new(),
        })
        .unwrap();
        db.issue_token(id, id).unwrap();
    }
    for id in ["a", "b"] {
        db.save_project(&tj_db::Project {
            id: id.into(),
            name: id.into(),
            repo_url: None,
            base_branch: "main".into(),
            workspace_path: None,
            git_provider: None,
            profile: "full".into(),
            workspace_kind: None,
            catalog_id: None,
            provisioning_state: None,
            provisioning_error: None,
            created_at: String::new(),
            updated_at: String::new(),
        })
        .unwrap();
        db.save_run(&tj_db::PipelineRun::new(
            format!("run-{id}"),
            Some(tj_db::ProjectId(id.into())),
            None,
            id.into(),
            RunStatus::Running,
            None,
            None,
            None,
            None,
        ))
        .unwrap();
    }
    db.set_project_member("admin", "a", "alice", true).unwrap();
    db.set_project_member("admin", "b", "bob", true).unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let handle = ManagerHandle {
        db: db.clone(),
        tx,
        pipeline_events: actor.pipeline_events.clone(),
        agent: Arc::new(tj_agent::Agent::new(
            "test".into(),
            Box::new(tj_provider::OpenAiProvider::new(
                "test",
                "http://127.0.0.1:1",
                "unused",
                "test",
            )),
            "test".into(),
            String::new(),
            &[],
        )),
        agent_sessions: Arc::new(DashMap::new()),
        web_bridges: actor.web_bridges.clone(),
        workspace_base: dir.to_string_lossy().into_owned(),
        config_loader: Arc::clone(&actor.config_loader),
        accepting_runs: Arc::new(std::sync::Mutex::new(true)),
        run_timeout_secs: None,
        operation_token: None,
        secrets_store: None,
    };
    assert!(
        SystemOperations::list_jobs(&handle).await.is_empty(),
        "unmapped chat integrations must not get system privileges"
    );
    let alice = handle.operations_for_token("alice".into());
    assert_eq!(SystemOperations::list_jobs(&alice).await.len(), 1);
    assert!(SystemOperations::get_job(&alice, "run-b").await.is_none());
    assert!(SystemOperations::cancel_job(&alice, "run-b").await.is_err());
    assert!(
        SystemOperations::approve_gate(&alice, "run-b", "gate-x", 0, "stale-key", true, None)
            .await
            .is_err()
    );
    let denied = SystemOperations::create_job(&alice, "test", Some("b"), None, None, None)
        .await
        .unwrap();
    assert!(denied.get("error").is_some());
    let denied =
        SystemOperations::create_project(&alice, "private", None, None, None, None, None, None)
            .await
            .unwrap();
    assert!(denied.get("error").is_some());
    assert!(
        rx.try_recv().is_err(),
        "denied commands must not enqueue work"
    );
    assert_eq!(db.list_runs().unwrap().len(), 3);
    db.set_project_member("admin", "a", "alice", false).unwrap();
    assert!(SystemOperations::list_jobs(&alice).await.is_empty());
    db.revoke_token("alice").unwrap();
    assert!(SystemOperations::list_flows(&alice).await.is_empty());
    drop(alice);
    drop(handle);
    drop(db);
    drop(actor);
    std::fs::remove_dir_all(dir).unwrap();
}

fn seed_gate(actor: &ManagerActor, gate_name: &str) -> i64 {
    actor
        .db
        .create_user(&tj_db::User {
            id: "admin".into(),
            username: "admin".into(),
            password_hash: "unused".into(),
            display_name: None,
            is_admin: true,
            created_at: String::new(),
        })
        .unwrap();
    let run = actor.db.load_run("run").unwrap().unwrap();
    actor
        .db
        .enter_gate(
            "run",
            run.state_version(),
            &format!("gate_{gate_name}"),
            gate_name,
            "fixture cause",
        )
        .unwrap()
}

fn decision(key: &str, gate: &str, approved: bool) -> tj_db::GateDecision {
    tj_db::GateDecision {
        id: String::new(),
        run_id: tj_db::RunId("run".into()),
        gate_name: String::new(),
        actor_id: "admin".into(),
        actor_name: "Admin".into(),
        approved,
        reason: if approved {
            None
        } else {
            Some("needs work".into())
        },
        note: None,
        created_at: String::new(),
        gate_instance_id: Some(gate.into()),
        idempotency_key: Some(key.into()),
        expected_state_version: None,
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
    }
}

#[tokio::test]
async fn manager_acknowledges_one_gate_transition_and_replays_without_rerouting() {
    let (mut actor, dir) = actor();
    let version = seed_gate(&actor, "gate-a");
    let request = decision("manager-once", "gate_gate-a", true);

    let receipt = actor
        .handle_gate_decision(request.clone(), version)
        .await
        .unwrap();
    assert!(!receipt.replayed);
    assert_eq!(
        receipt.status,
        RunStatus::Running,
        "receipt records the durable decision transition"
    );
    assert_eq!(
        *actor.db.load_run("run").unwrap().unwrap().status(),
        RunStatus::Completed
    );
    assert_eq!(actor.db.list_gate_decisions("run").unwrap().len(), 1);
    let events = actor.db.list_events("run").unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "gate_decision")
            .count(),
        1
    );
    let event_count = events.len();

    let replay = actor.handle_gate_decision(request, version).await.unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.id, receipt.id);
    assert_eq!(actor.db.list_gate_decisions("run").unwrap().len(), 1);
    assert_eq!(
        actor.db.list_events("run").unwrap().len(),
        event_count,
        "replay must not emit or route again"
    );
    drop(actor);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn manager_rejects_stale_gate_submissions_before_any_effect() {
    let (mut actor, dir) = actor();
    let version = seed_gate(&actor, "gate-a");
    let events = actor.db.list_events("run").unwrap().len();

    let stale_version = decision("manager-stale-version", "gate_gate-a", true);
    let error = actor
        .handle_gate_decision(stale_version, version + 1)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("Gate conflict"), "{error}");
    let stale_gate = decision("manager-stale-gate", "gate_other", false);
    assert!(
        actor
            .handle_gate_decision(stale_gate, version)
            .await
            .is_err()
    );

    let run = actor.db.load_run("run").unwrap().unwrap();
    assert_eq!(*run.status(), RunStatus::WaitingForGate);
    assert_eq!(run.gate_instance_id(), Some("gate_gate-a"));
    assert_eq!(run.state_version(), version);
    assert!(actor.db.list_gate_decisions("run").unwrap().is_empty());
    assert_eq!(actor.db.list_events("run").unwrap().len(), events);
    drop(actor);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn manager_accepts_question_bound_web_input_once_and_rejects_stale_plain_replies() {
    let (actor, dir) = actor();
    let db = actor.db.clone();
    let flow = tj_db::FlowRecord::new(
        "flow_api".into(),
        tj_db::RunId("run".into()),
        "flow-a".into(),
        Some(1),
        None,
        None,
    );
    db.save_flow(&flow).unwrap();
    let mut stage = tj_db::StageRecord::new(
        "stage_api".into(),
        tj_db::FlowId("flow_api".into()),
        "start".into(),
        tj_db::StageStatus::Running,
        2,
        None,
    );
    db.save_stage(&stage).unwrap();
    stage.set_status(tj_db::StageStatus::WaitingForInput);
    db.save_stage(&stage).unwrap();
    let question = tj_db::QuestionRecord::new(
        "question_api".into(),
        tj_db::RunId("run".into()),
        "flow_api".into(),
        "stage_api".into(),
        "start".into(),
        "flow-a".into(),
        2,
        "{}".into(),
    );
    db.create_question(&question).unwrap();

    let handle = ManagerHandle {
        db: db.clone(),
        tx: actor.tx.clone(),
        pipeline_events: actor.pipeline_events.clone(),
        agent: Arc::new(tj_agent::Agent::new(
            "test".into(),
            Box::new(tj_provider::OpenAiProvider::new(
                "test",
                "http://127.0.0.1:1",
                "unused",
                "test",
            )),
            "test".into(),
            String::new(),
            &[],
        )),
        agent_sessions: Arc::new(DashMap::new()),
        web_bridges: actor.web_bridges.clone(),
        workspace_base: dir.to_string_lossy().into_owned(),
        config_loader: Arc::clone(&actor.config_loader),
        accepting_runs: Arc::new(std::sync::Mutex::new(true)),
        run_timeout_secs: None,
        operation_token: None,
        secrets_store: None,
    };
    let receipt = handle
        .send_web_input(
            "run",
            r#"{"selected":["a"]}"#,
            Some("stage_api"),
            Some("question_api"),
            Some(2),
            Some("api-key"),
        )
        .unwrap();
    assert_eq!(receipt.status, "accepted");
    assert!(!receipt.replayed);
    assert_eq!(receipt.question_id.as_deref(), Some("question_api"));
    let replay = handle
        .send_web_input(
            "run",
            r#"{"selected":["a"]}"#,
            Some("stage_api"),
            Some("question_api"),
            Some(2),
            Some("api-key"),
        )
        .unwrap();
    assert!(replay.replayed);
    let stale = handle
        .send_web_input(
            "run",
            r#"{"selected":["b"]}"#,
            Some("stage_api"),
            Some("question_api"),
            Some(2),
            Some("other-key"),
        )
        .unwrap_err();
    assert!(stale.contains("already answered"), "{stale}");

    // A new open question makes unaddressed plain input fail closed.
    let mut stage2 = tj_db::StageRecord::new(
        "stage_api2".into(),
        tj_db::FlowId("flow_api".into()),
        "start".into(),
        tj_db::StageStatus::Running,
        0,
        None,
    );
    db.save_stage(&stage2).unwrap();
    stage2.set_status(tj_db::StageStatus::WaitingForInput);
    db.save_stage(&stage2).unwrap();
    let question2 = tj_db::QuestionRecord::new(
        "question_api2".into(),
        tj_db::RunId("run".into()),
        "flow_api".into(),
        "stage_api2".into(),
        "start".into(),
        "flow-a".into(),
        0,
        "{}".into(),
    );
    db.create_question(&question2).unwrap();
    let error = handle
        .send_web_input("run", "plain", Some("stage_api2"), None, None, None)
        .unwrap_err();
    assert!(error.contains("question_id"), "{error}");

    drop(handle);
    drop(actor);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn runtime_resolution_uses_the_frozen_bundle_and_fails_if_it_disappears() {
    let (actor, dir) = actor();
    let bundle = tj_config::bundle::ExecutionBundle::build(
        &actor.config_loader,
        "bundled",
        None,
        &actor_graph(&actor),
    )
    .unwrap();
    let record = tj_db::ExecutionBundleRecord {
        id: "bundle_test".into(),
        run_id: tj_db::RunId("bundled".into()),
        workflow_id: None,
        bundle_hash: bundle.bundle_hash.clone(),
        bundle_json: serde_json::to_string(&bundle).unwrap(),
        legacy: false,
        created_at: bundle.created_at.clone(),
    };
    actor.db.save_execution_bundle(&record).unwrap();
    let mut run = tj_db::PipelineRun::new(
        "bundled".into(),
        None,
        None,
        "bundled".into(),
        RunStatus::Running,
        None,
        None,
        None,
        None,
    );
    run.set_execution_bundle(Some(record.id.clone()), Some(record.bundle_hash.clone()));
    actor.db.save_run(&run).unwrap();

    let resolved = actor.smg_for_run("bundled").unwrap();
    assert_eq!(resolved.name, actor_graph(&actor).name);
    assert!(
        !actor
            .db
            .list_events("bundled")
            .unwrap()
            .iter()
            .any(|e| e.kind == "legacy_execution_bundle")
    );

    actor.db.delete_execution_bundle_for_run("bundled").unwrap();
    actor.smg_cache.lock().unwrap().clear();
    let error = actor.smg_for_run("bundled").unwrap_err().to_string();
    assert!(error.contains("missing execution bundle"), "{error}");
    drop(actor);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn runtime_resolution_fails_closed_for_missing_pins_and_unknown_flows() {
    let (actor, dir) = actor();
    let mut pinned = tj_db::PipelineRun::new(
        "pinned".into(),
        Some(tj_db::ProjectId("project".into())),
        None,
        "pinned".into(),
        RunStatus::Running,
        None,
        None,
        None,
        None,
    );
    pinned.set_workflow_id(Some("missing-workflow".into()));
    actor.db.save_run(&pinned).unwrap();
    let error = actor.smg_for_run("pinned").unwrap_err().to_string();
    assert!(
        error.contains("missing workflow 'missing-workflow'"),
        "{error}"
    );

    let error = actor
        .next_flow("run", "not-in-flow-order")
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("not in workflow 'synthetic-workflow' flow-order"),
        "{error}"
    );
    drop(actor);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn workflow_import_fails_closed_on_invalid_graph_and_references() {
    let (actor, dir) = actor();
    let tree = tj_test_support::SyntheticTree::new();
    let smg_dir = tree.root().join("sources");
    std::fs::create_dir_all(&smg_dir).unwrap();
    tree.update_config(|config| {
        config["workflow_sources"] =
            serde_json::json!([{"id": "synthetic-sources", "path": "sources", "kind": "directory"}]);
    })
    .unwrap();
    let source = serde_json::to_value(actor_graph(&actor))
        .unwrap()
        .to_string();
    let mut invalid: serde_json::Value = serde_json::from_str(&source).unwrap();
    invalid["flows"]["flow-a"]["stages"]["start"]["routes"]["success"] =
        serde_json::json!("missing-stage");
    let invalid_path = smg_dir.join("invalid.smg.json");
    std::fs::write(&invalid_path, invalid.to_string()).unwrap();
    let loader = ConfigLoader::from_file(tree.config_path()).unwrap();
    let error = format!("{:#}", loader.load_workflow_sources().unwrap_err());
    assert!(
        error.contains("invalid.smg.json") && error.contains("unknown stage"),
        "{error}"
    );
    assert!(
        actor.db.list_workflows().unwrap().is_empty(),
        "validation happens before any registry write"
    );
    std::fs::remove_file(&invalid_path).unwrap();

    let mut missing_reference: serde_json::Value = serde_json::from_str(&source).unwrap();
    missing_reference["flows"]["flow-a"]["stages"]["start"]["agent"] =
        serde_json::json!("config/stages/missing/agent.json");
    std::fs::write(
        smg_dir.join("missing.smg.json"),
        missing_reference.to_string(),
    )
    .unwrap();
    let error = format!("{:#}", loader.load_workflow_sources().unwrap_err());
    assert!(error.contains("agent/ADC reference"), "{error}");
    assert!(actor.db.list_workflows().unwrap().is_empty());
    drop(actor);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn duplicate_completion_does_not_route_or_emit_twice() {
    let (mut actor, dir) = actor();
    let attempt = actor
        .db
        .begin_flow_attempt("run", "flow-a", "flow")
        .unwrap();
    actor.handle(completion(&attempt)).await;
    assert_eq!(
        *actor.db.load_run("run").unwrap().unwrap().status(),
        RunStatus::Failed
    );
    let events = actor.db.list_events("run").unwrap().len();
    actor.handle(completion(&attempt)).await;
    assert_eq!(actor.db.list_events("run").unwrap().len(), events);
    drop(actor);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn old_attempt_cannot_fail_or_route_a_replacement() {
    let (mut actor, dir) = actor();
    let old = actor
        .db
        .begin_flow_attempt("run", "flow-a", "flow")
        .unwrap();
    actor.db.abandon_execution_on_restart("run").unwrap();
    let new = actor
        .db
        .begin_flow_attempt("run", "flow-a", "new-flow")
        .unwrap();
    let events = actor.db.list_events("run").unwrap().len();
    actor.handle(completion(&old)).await;
    assert_eq!(
        *actor.db.load_run("run").unwrap().unwrap().status(),
        RunStatus::Running
    );
    assert_eq!(actor.db.list_events("run").unwrap().len(), events);
    assert!(
        actor
            .db
            .accept_flow_completion("run", "flow-a", &new)
            .unwrap()
    );
    drop(actor);
    std::fs::remove_dir_all(dir).unwrap();
}

struct Dropped(Arc<std::sync::atomic::AtomicBool>);
impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

async fn pending_execution(actor: &mut ManagerActor) -> Arc<std::sync::atomic::AtomicBool> {
    let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let guard = Dropped(dropped.clone());
    let (ready, started) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let _guard = guard;
        let _ = ready.send(());
        std::future::pending::<()>().await;
    });
    started.await.unwrap();
    actor.executions.insert(
        "run".into(),
        ActiveExecution {
            task,
            processes: tj_tools::processes::ProcessSupervisor::default(),
        },
    );
    dropped
}

#[tokio::test]
async fn cancellation_acknowledges_only_after_execution_future_is_dropped() {
    let (mut actor, dir) = actor();
    let dropped = pending_execution(&mut actor).await;
    let attempt = actor
        .db
        .begin_flow_attempt("run", "flow-a", "flow")
        .unwrap();
    let (reply, acknowledged) = tokio::sync::oneshot::channel();
    actor
        .handle(ManagerEvent::CancelRun {
            run_id: "run".into(),
            reply,
        })
        .await;
    acknowledged.await.unwrap().unwrap();
    assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
    assert!(!actor.executions.contains_key("run"));
    assert_eq!(
        *actor.db.load_run("run").unwrap().unwrap().status(),
        RunStatus::Cancelled
    );
    let count = actor.db.list_events("run").unwrap().len();
    actor.handle(completion(&attempt)).await;
    actor.handle_cancel_run("run").await.unwrap();
    assert_eq!(actor.db.list_events("run").unwrap().len(), count);
    drop(actor);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn shutdown_uses_the_same_joined_cancellation_path() {
    let (mut actor, dir) = actor();
    let dropped = pending_execution(&mut actor).await;
    let (reply, acknowledged) = tokio::sync::oneshot::channel();
    actor.handle(ManagerEvent::Shutdown { reply }).await;
    acknowledged.await.unwrap().unwrap();
    assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(
        *actor.db.load_run("run").unwrap().unwrap().status(),
        RunStatus::Cancelled
    );
    drop(actor);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn failed_execution_join_preserves_workspace_and_returns_error() {
    let (mut actor, dir) = actor();
    let task = tokio::spawn(async {
        panic!("injected execution failure");
    });
    while !task.is_finished() {
        tokio::task::yield_now().await;
    }
    actor.executions.insert(
        "run".into(),
        ActiveExecution {
            task,
            processes: tj_tools::processes::ProcessSupervisor::default(),
        },
    );
    actor.worktrees.insert(
        "run".into(),
        WorktreeInfo {
            worktree_path: dir.clone(),
            repo_path: dir.clone(),
            branch_name: "test".into(),
        },
    );
    assert!(actor.handle_cancel_run("run").await.is_err());
    actor.cleanup_task_worktree("run").await;
    assert!(actor.cleanup_blocked.contains("run"));
    assert!(actor.worktrees.contains_key("run"));
    assert!(dir.exists());
    assert!(
        actor
            .db
            .list_events("run")
            .unwrap()
            .iter()
            .any(|event| event.kind == "execution_shutdown_failed")
    );
    drop(actor);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn cancellation_ack_waits_for_actual_shell_descendants() {
    use tj_tools::Tool;
    let (mut actor, dir) = actor();
    let processes = tj_tools::processes::ProcessSupervisor::default();
    let scope = processes.clone();
    let workspace = dir.clone();
    let task = tokio::spawn(async move {
        scope.scope(async move {
            let ctx = tj_tools::ToolContext { workspace: workspace.clone(), output_dir: workspace };
            tj_tools::ShellTool::new().execute(
                serde_json::json!({"command": "sleep 30 & echo $! > child; echo ready > started; wait"}), &ctx,
            ).await;
        }).await;
    });
    actor
        .executions
        .insert("run".into(), ActiveExecution { task, processes });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !dir.join("started").exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let pid = std::fs::read_to_string(dir.join("child")).unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        actor.handle_cancel_run("run"),
    )
    .await
    .unwrap()
    .unwrap();
    #[cfg(target_os = "linux")]
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{}/stat", pid.trim())) {
        let state = stat
            .rsplit_once(')')
            .unwrap()
            .1
            .split_whitespace()
            .next()
            .unwrap();
        assert!(matches!(state, "Z" | "X"), "child is still alive: {stat}");
    }
    drop(actor);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn old_watchdog_cannot_terminate_rearmed_execution() {
    let (mut actor, dir) = actor();
    let dropped = pending_execution(&mut actor).await;
    actor.start_watchdog("run", Some(60));
    let old = actor.watchdogs["run"].generation.clone();
    actor.start_watchdog("run", Some(60));
    let current = actor.watchdogs["run"].generation.clone();
    assert_ne!(old, current);
    actor.handle_watchdog_tick("run", &old).await;
    assert_eq!(
        *actor.db.load_run("run").unwrap().unwrap().status(),
        RunStatus::Running
    );
    assert!(!dropped.load(std::sync::atomic::Ordering::SeqCst));
    actor.handle_watchdog_tick("run", &current).await;
    assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(
        *actor.db.load_run("run").unwrap().unwrap().status(),
        RunStatus::Failed
    );
    assert!(!actor.watchdogs.contains_key("run"));
    drop(actor);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn watchdog_during_question_fences_late_answer_and_joins_waiter() {
    let (mut actor, dir) = actor();
    actor
        .db
        .save_flow(&tj_db::FlowRecord::new(
            "flow".into(),
            tj_db::RunId("run".into()),
            "flow-a".into(),
            None,
            None,
            None,
        ))
        .unwrap();
    let mut stage = tj_db::StageRecord::new(
        "question".into(),
        tj_db::FlowId("flow".into()),
        "start".into(),
        tj_db::StageStatus::WaitingForInput,
        0,
        None,
    );
    actor.db.save_stage(&stage).unwrap();
    let dropped = pending_execution(&mut actor).await;
    actor.start_watchdog("run", Some(60));
    let generation = actor.watchdogs["run"].generation.clone();
    actor.handle_watchdog_tick("run", &generation).await;
    assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
    stage.set_status(tj_db::StageStatus::Running);
    assert!(actor.db.save_stage(&stage).is_err());
    assert_eq!(
        *actor.db.load_stage("question").unwrap().unwrap().status(),
        tj_db::StageStatus::Failed
    );
    drop(actor);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn absolute_deadline_applies_during_gate_wait() {
    let (mut actor, dir) = actor();
    let mut run = tj_db::PipelineRun::new(
        "deadline".into(),
        None,
        None,
        "deadline".into(),
        RunStatus::WaitingForGate,
        None,
        None,
        None,
        None,
    );
    run.set_deadline_at(Some("2000-01-01T00:00:00Z".into()));
    actor.db.save_run(&run).unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    actor.tx = tx;
    actor.start_watchdog("deadline", None);
    assert_eq!(
        actor.watchdogs["deadline"].reason,
        "absolute run deadline elapsed"
    );
    let tick = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .unwrap()
        .unwrap();
    actor.handle(tick).await;
    assert_eq!(
        *actor.db.load_run("deadline").unwrap().unwrap().status(),
        RunStatus::Failed
    );
    actor.start_watchdog("run", None);
    assert!(
        !actor.watchdogs.contains_key("run"),
        "unconfigured gate deadline should preserve existing policy"
    );
    drop(actor);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn returned_waiting_outcome_never_advances_as_success() {
    let (mut actor, dir) = actor();
    let attempt = actor
        .db
        .begin_flow_attempt("run", "flow-a", "flow")
        .unwrap();
    actor
        .handle(ManagerEvent::FlowDone {
            attempt_id: attempt,
            run_id: "run".into(),
            flow_name: "flow-a".into(),
            trace_id: "test".into(),
            outcome: FlowOutcome::WaitingForInput {
                flow_id: "flow".into(),
                run_id: "run".into(),
                flow_name: "flow-a".into(),
                stage_name: "start".into(),
                stage_id: "stage".into(),
                message: "unanswered question".into(),
            },
        })
        .await;
    assert_eq!(
        *actor.db.load_run("run").unwrap().unwrap().status(),
        RunStatus::Failed
    );
    assert!(
        !actor
            .db
            .list_events("run")
            .unwrap()
            .iter()
            .any(|event| event.kind == "run_completed" || event.kind == "flow_completed")
    );
    drop(actor);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn resume_path_arms_its_own_watchdog() {
    let (mut actor, dir) = actor();
    actor
        .db
        .save_flow(&tj_db::FlowRecord::new(
            "flow".into(),
            tj_db::RunId("run".into()),
            "flow-a".into(),
            None,
            None,
            None,
        ))
        .unwrap();
    actor
        .spawn_flow_resume(
            SpawnFlowParams {
                run_id: "run".into(),
                flow_name: "flow-a".into(),
                description: "test".into(),
                existing_artifacts: HashMap::new(),
                workspace_path: None,
                branch_name: None,
                slug: "test".into(),
                attempt_offset: 0,
            },
            "start",
            "flow",
        )
        .await;
    assert!(actor.watchdogs.contains_key("run"));
    actor.handle_cancel_run("run").await.unwrap();
    drop(actor);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn cancellation_before_publication_preserves_uncommitted_task_workspace() {
    let (mut actor, dir) = actor();
    let workspace = dir.join("task-worktree");
    std::fs::create_dir_all(&workspace).unwrap();
    let init = std::process::Command::new("git")
        .args(["init"])
        .current_dir(&workspace)
        .output()
        .unwrap();
    assert!(init.status.success());
    std::fs::write(workspace.join("change.rs"), "unpublished work").unwrap();
    actor.worktrees.insert(
        "run".into(),
        WorktreeInfo {
            worktree_path: workspace.clone(),
            repo_path: workspace.clone(),
            branch_name: "tj/run-task".into(),
        },
    );
    actor.cleanup_task_worktree("run").await;
    assert!(
        workspace.join("change.rs").exists(),
        "unpublished workspace was deleted"
    );
    assert!(actor.worktrees.contains_key("run"));
    drop(actor);
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(feature = "example-workflows")]
#[tokio::test]
async fn task_selection_event_cannot_start_engineering_without_identity_or_wait_state() {
    let (mut actor, dir) = actor();
    // Not waiting for task selection: an arbitrary event must be ignored.
    actor.handle_task_selected("run", "missing-selection").await;
    assert!(actor.db.load_flows_for_run("run").unwrap().is_empty());
    assert_eq!(
        actor.db.load_run("run").unwrap().unwrap().status(),
        &RunStatus::Running
    );

    // Waiting marker, but no durable plan/selection identity: still no spawn.
    let mut run = actor.db.load_run("run").unwrap().unwrap();
    run.set_status(RunStatus::WaitingForGate);
    run.set_gate_state(Some("engineering_task_selection_required".into()));
    run.set_updated_at(chrono::Utc::now().to_rfc3339());
    actor.db.save_run(&run).unwrap();
    actor.handle_task_selected("run", "missing-selection").await;
    assert!(actor.db.load_flows_for_run("run").unwrap().is_empty());
    assert_eq!(
        actor.db.load_run("run").unwrap().unwrap().status(),
        &RunStatus::WaitingForGate
    );
    drop(actor);
    std::fs::remove_dir_all(dir).unwrap();
}
