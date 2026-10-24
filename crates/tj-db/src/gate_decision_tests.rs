use super::*;
use std::sync::{Arc, Barrier};

fn fixture() -> Database {
    let db = Database {
        conn: Mutex::new(rusqlite::Connection::open_in_memory().unwrap()),
    };
    db.migrate().unwrap();
    for id in ["admin", "alice", "bob"] {
        db.create_user(&User {
            id: id.into(),
            username: id.into(),
            password_hash: "unused".into(),
            display_name: None,
            is_admin: id == "admin",
            created_at: String::new(),
        })
        .unwrap();
    }
    db.save_project(&Project {
        id: "project".into(),
        name: "project".into(),
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
    db.set_project_member("admin", "project", "alice", true)
        .unwrap();
    db.save_run(&PipelineRun::new(
        "run".into(),
        Some(ProjectId("project".into())),
        None,
        "gate test".into(),
        RunStatus::Running,
        None,
        None,
        None,
        None,
    ))
    .unwrap();
    db
}

fn decision(key: &str, gate: &str, approved: bool) -> GateDecision {
    GateDecision {
        id: String::new(),
        run_id: RunId("run".into()),
        gate_name: String::new(),
        actor_id: "alice".into(),
        actor_name: "Alice".into(),
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

fn enter(db: &Database, gate: &str) -> i64 {
    let run = db.load_run("run").unwrap().unwrap();
    db.enter_gate(
        "run",
        run.state_version(),
        gate,
        "gate-a",
        "review the packet",
    )
    .unwrap()
}

#[test]
fn gate_entry_persists_identity_and_rejects_a_second_entry() {
    let db = fixture();
    let version = enter(&db, "gate_one");
    let run = db.load_run("run").unwrap().unwrap();
    assert_eq!(*run.status(), RunStatus::WaitingForGate);
    assert_eq!(run.gate_instance_id(), Some("gate_one"));
    assert_eq!(run.gate_name(), Some("gate-a"));
    assert_eq!(run.state_version(), version);
    assert_eq!(run.gate_state(), Some("review the packet"));
    let events = db.list_events("run").unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "gate_reached"
            && event.flow_name.as_deref() == Some("gate-a")
            && event
                .payload
                .as_deref()
                .is_some_and(|payload| payload.contains("gate_one"))
    }));

    // A duplicate GateReached cannot overwrite an active gate identity.
    assert!(
        db.enter_gate("run", version, "gate_two", "gate-a", "duplicate")
            .is_err()
    );
    let run = db.load_run("run").unwrap().unwrap();
    assert_eq!(run.gate_instance_id(), Some("gate_one"));
    assert_eq!(run.state_version(), version);
}

#[test]
fn retry_returns_the_same_receipt_without_duplicating_history() {
    let db = fixture();
    let version = enter(&db, "gate_one");
    let request = decision("idem-one", "gate_one", true);
    let accepted = db.decide_gate(&request, version, 3).unwrap();
    assert!(!accepted.replayed);
    assert_eq!(accepted.disposition, "approved");
    assert_eq!(accepted.status, RunStatus::Running);
    assert_eq!(accepted.state_version, version + 1);
    assert_eq!(
        *db.load_run("run").unwrap().unwrap().status(),
        RunStatus::Running
    );

    let decisions = db.list_gate_decisions("run").unwrap();
    let events = db.list_events("run").unwrap().len();
    let replayed = db.decide_gate(&request, version, 3).unwrap();
    assert!(replayed.replayed);
    assert_eq!(replayed.id, accepted.id);
    assert_eq!(replayed.state_version, accepted.state_version);
    assert_eq!(replayed.disposition, accepted.disposition);
    assert_eq!(
        db.list_gate_decisions("run").unwrap().len(),
        decisions.len()
    );
    assert_eq!(db.list_events("run").unwrap().len(), events);

    let mut changed = request.clone();
    changed.reason = Some("different intent".into());
    assert!(db.decide_gate(&changed, version, 3).is_err());
    assert_eq!(db.list_gate_decisions("run").unwrap().len(), 1);
}

#[test]
fn stale_gate_and_version_are_rejected_without_effects() {
    let db = fixture();
    let version = enter(&db, "gate_one");

    assert!(
        db.decide_gate(&decision("wrong-gate", "gate_other", true), version, 3)
            .is_err()
    );
    assert!(
        db.decide_gate(&decision("wrong-version", "gate_one", true), version - 1, 3)
            .is_err()
    );
    assert!(db.list_gate_decisions("run").unwrap().is_empty());
    let run = db.load_run("run").unwrap().unwrap();
    assert_eq!(*run.status(), RunStatus::WaitingForGate);
    assert_eq!(run.state_version(), version);

    let accepted = db
        .decide_gate(&decision("correct", "gate_one", true), version, 3)
        .unwrap();
    assert!(!accepted.replayed);
    assert_eq!(db.list_gate_decisions("run").unwrap().len(), 1);
}

#[test]
fn generic_output_gate_requires_exact_durable_identity_and_reports_normalized_kind() {
    let db = fixture();
    let artifact = RunArtifact {
        id: "art_review".into(),
        run_id: RunId("run".into()),
        flow_name: "flow-a".into(),
        stage_name: "finish".into(),
        attempt: 0,
        version: 1,
        content: r#"{"summary":"reviewed"}"#.into(),
        content_hash: "a".repeat(64),
        created_at: "2026-10-22T00:00:00+00:00".into(),
    };
    db.append_artifact(&artifact).unwrap();
    let run = db.load_run("run").unwrap().unwrap();
    db.enter_gate_bound_to_output(
        "run",
        run.state_version(),
        "gate_output",
        "gate-b",
        "review the exact output",
        None,
        None,
        Some(&artifact),
    )
    .unwrap();
    let binding = db.gate_output_binding("gate_output").unwrap().unwrap();
    assert_eq!(binding.artifact_id, artifact.id);
    assert_eq!(binding.artifact_version, artifact.version);
    assert_eq!(binding.content_hash, artifact.content_hash);
    assert_eq!(binding.flow_name, "flow-a");
    assert_eq!(binding.stage_name, "finish");

    let version = db.load_run("run").unwrap().unwrap().state_version();
    let mut stale = decision("output-stale", "gate_output", true);
    stale.output_artifact_id = Some("art_other".into());
    stale.output_content_hash = Some("b".repeat(64));
    stale.output_stage_name = Some("finish".into());
    let error = db.decide_gate(&stale, version, 3).unwrap_err().to_string();
    assert!(error.contains("stale or missing"), "{error}");
    assert!(db.decide_gate(&stale, version, 3).is_err());
    assert!(db.list_gate_decisions("run").unwrap().is_empty());

    let mut accepted = decision("output-exact", "gate_output", true);
    accepted.output_artifact_id = Some(artifact.id.clone());
    accepted.output_content_hash = Some(artifact.content_hash.clone());
    accepted.output_stage_name = Some(artifact.stage_name.clone());
    let receipt = db.decide_gate(&accepted, version, 3).unwrap();
    assert_eq!(receipt.gate_binding_kind.as_deref(), Some("artifact"));
    assert_eq!(receipt.output_artifact_id.as_deref(), Some("art_review"));
    let replay = db.decide_gate(&accepted, version, 3).unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.gate_binding_kind.as_deref(), Some("artifact"));
}

#[test]
fn unauthorized_decisions_have_no_effects() {
    let db = fixture();
    let version = enter(&db, "gate_one");
    let mut request = decision("bob-key", "gate_one", false);
    request.actor_id = "bob".into();
    let error = db
        .decide_gate(&request, version, 3)
        .unwrap_err()
        .to_string();
    assert!(error.to_lowercase().contains("access denied"), "{error}");
    assert_eq!(
        *db.load_run("run").unwrap().unwrap().status(),
        RunStatus::WaitingForGate
    );
    assert!(db.list_gate_decisions("run").unwrap().is_empty());
}

#[test]
fn concurrent_decisions_accept_exactly_one_per_gate_instance() {
    let path = std::env::temp_dir().join(format!("tj-gate-race-{}.db", uuid::Uuid::new_v4()));
    let first = Database::open(path.to_str().unwrap()).unwrap();
    for id in ["admin", "alice"] {
        first
            .create_user(&User {
                id: id.into(),
                username: id.into(),
                password_hash: "unused".into(),
                display_name: None,
                is_admin: id == "admin",
                created_at: String::new(),
            })
            .unwrap();
    }
    first
        .save_project(&Project {
            id: "project".into(),
            name: "project".into(),
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
    first
        .set_project_member("admin", "project", "alice", true)
        .unwrap();
    first
        .save_run(&PipelineRun::new(
            "run".into(),
            Some(ProjectId("project".into())),
            None,
            "race".into(),
            RunStatus::Running,
            None,
            None,
            None,
            None,
        ))
        .unwrap();
    let version = enter(&first, "gate_race");
    let second = Database::open(path.to_str().unwrap()).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = [(first, "key-a", true), (second, "key-b", false)]
        .into_iter()
        .map(|(db, key, approved)| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                db.decide_gate(&decision(key, "gate_race", approved), version, 3)
            })
        })
        .collect();
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    let accepted = results.iter().filter(|result| result.is_ok()).count();
    assert_eq!(
        accepted, 1,
        "exactly one concurrent decision may be accepted: {:?}",
        results
    );
    let db = Database::open(path.to_str().unwrap()).unwrap();
    assert_eq!(db.list_gate_decisions("run").unwrap().len(), 1);
    assert_eq!(
        *db.load_run("run").unwrap().unwrap().status(),
        RunStatus::Running
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn rejection_limit_counts_only_accepted_rejections_and_duplicate_retries() {
    let db = fixture();
    let mut version = enter(&db, "gate_one");
    let first = db
        .decide_gate(&decision("reject-one", "gate_one", false), version, 3)
        .unwrap();
    assert_eq!(first.disposition, "rework");
    assert_eq!(first.status, RunStatus::Running);

    // A retry of an already accepted rejection must not consume another slot.
    let replayed = db
        .decide_gate(&decision("reject-one", "gate_one", false), version, 3)
        .unwrap();
    assert!(replayed.replayed);
    assert_eq!(replayed.disposition, "rework");

    version = enter(&db, "gate_two");
    assert_eq!(
        db.decide_gate(&decision("reject-two", "gate_two", false), version, 3)
            .unwrap()
            .disposition,
        "rework"
    );
    version = enter(&db, "gate_three");
    let final_rejection = db
        .decide_gate(&decision("reject-three", "gate_three", false), version, 3)
        .unwrap();
    assert_eq!(final_rejection.disposition, "rework_limit_cancelled");
    assert_eq!(final_rejection.status, RunStatus::Cancelled);
    assert_eq!(
        *db.load_run("run").unwrap().unwrap().status(),
        RunStatus::Cancelled
    );
    assert_eq!(db.list_gate_decisions("run").unwrap().len(), 3);

    // The cancellation is durable and its retry does not create a fourth decision.
    assert!(
        db.decide_gate(&decision("reject-three", "gate_three", false), version, 3)
            .unwrap()
            .replayed
    );
    assert_eq!(db.list_gate_decisions("run").unwrap().len(), 3);
}

#[test]
fn failed_decision_persistence_never_advances_the_run() {
    let db = fixture();
    let version = enter(&db, "gate_one");
    let events = db.list_events("run").unwrap().len();
    lock_conn!(db).execute_batch(
        "CREATE TRIGGER reject_gate_decision BEFORE INSERT ON gate_decisions BEGIN SELECT RAISE(ABORT, 'injected decision failure'); END;"
    ).unwrap();
    assert!(
        db.decide_gate(&decision("injected", "gate_one", true), version, 3)
            .is_err()
    );
    let run = db.load_run("run").unwrap().unwrap();
    assert_eq!(*run.status(), RunStatus::WaitingForGate);
    assert_eq!(run.gate_instance_id(), Some("gate_one"));
    assert_eq!(run.state_version(), version);
    assert!(db.list_gate_decisions("run").unwrap().is_empty());
    assert_eq!(db.list_events("run").unwrap().len(), events);
}
