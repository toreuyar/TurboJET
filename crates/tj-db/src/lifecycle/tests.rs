use super::*;
use std::sync::{Arc, Barrier};

fn fixture() -> Database {
    let db = Database {
        conn: Mutex::new(rusqlite::Connection::open_in_memory().unwrap()),
    };
    db.migrate().unwrap();
    seed(&db);
    db
}

fn seed(db: &Database) {
    db.save_run(&PipelineRun::new(
        "run".into(),
        None,
        None,
        "fixture".into(),
        RunStatus::Running,
        None,
        None,
        None,
        None,
    ))
    .unwrap();
    db.save_flow(&FlowRecord::new(
        "flow".into(),
        RunId("run".into()),
        "flow-a".into(),
        None,
        None,
        None,
    ))
    .unwrap();
    db.save_stage(&StageRecord::new(
        "stage".into(),
        FlowId("flow".into()),
        "stage-a".into(),
        StageStatus::Running,
        0,
        None,
    ))
    .unwrap();
}

#[test]
fn stale_writers_conflict_even_when_status_did_not_change() {
    let db = fixture();
    let mut first = db.load_stage("stage").unwrap().unwrap();
    let mut stale = first.clone();
    first.set_output_artifacts(Some("first".into()));
    db.save_stage(&first).unwrap();
    stale.set_output_artifacts(Some("stale".into()));
    assert!(db.save_stage(&stale).is_err());
    assert_eq!(
        db.load_stage("stage").unwrap().unwrap().output_artifacts(),
        Some("first")
    );
}

#[test]
fn terminal_run_fences_descendants_and_question_restores_atomically() {
    let db = fixture();
    let mut late_stage = db.load_stage("stage").unwrap().unwrap();
    let mut late_flow = db.load_flow("flow").unwrap().unwrap();
    let mut run = db.load_run("run").unwrap().unwrap();
    let mut late_run = run.clone();
    run.set_status(RunStatus::Cancelled);
    db.save_run(&run).unwrap();
    late_stage.set_status(StageStatus::Completed);
    late_flow.set_status(FlowStatus::Completed);
    late_run.set_status(RunStatus::Completed);
    assert!(db.save_stage(&late_stage).is_err());
    assert!(db.save_flow(&late_flow).is_err());
    assert!(db.save_run(&late_run).is_err());
    let mut fresh = db.load_stage("stage").unwrap().unwrap();
    fresh.set_status(StageStatus::Running);
    assert!(db.save_stage(&fresh).is_err());
    assert_eq!(
        *db.load_flow("flow").unwrap().unwrap().status(),
        FlowStatus::Failed
    );
    assert_eq!(
        *db.load_stage("stage").unwrap().unwrap().status(),
        StageStatus::Failed
    );
    assert_eq!(db.list_events("run").unwrap().len(), 6);
}

#[test]
fn audit_failure_rolls_back_state_and_revision() {
    let db = fixture();
    lock_conn!(db).execute_batch("CREATE TRIGGER reject_events BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT, 'injected audit failure'); END;").unwrap();
    let mut row = db.load_stage("stage").unwrap().unwrap();
    row.set_status(StageStatus::Completed);
    assert!(db.save_stage(&row).is_err());
    assert_eq!(
        *db.load_stage("stage").unwrap().unwrap().status(),
        StageStatus::Running
    );
    lock_conn!(db)
        .execute_batch("DROP TRIGGER reject_events;")
        .unwrap();
    db.save_stage(&row).unwrap();
    assert_eq!(
        *db.load_stage("stage").unwrap().unwrap().status(),
        StageStatus::Completed
    );
}

#[test]
fn deleted_rows_cannot_be_recreated_by_in_flight_writers() {
    let db = fixture();
    let stage = db.load_stage("stage").unwrap().unwrap();
    let run = db.load_run("run").unwrap().unwrap();
    db.delete_run_cascade("run").unwrap();
    assert!(db.save_run(&run).is_err());
    assert!(db.save_stage(&stage).is_err());
    assert!(db.complete_flow("flow").is_err());
    assert!(db.load_run("run").unwrap().is_none());
}

#[test]
fn completion_claim_is_durable_single_use_and_attempt_scoped() {
    let db = fixture();
    let first = db.begin_flow_attempt("run", "flow-a", "flow").unwrap();
    assert!(db.begin_flow_attempt("run", "flow-a", "another").is_err());
    assert!(
        !db.accept_flow_completion("run", "wrong-flow", &first)
            .unwrap()
    );
    assert!(db.accept_flow_completion("run", "flow-a", &first).unwrap());
    assert!(!db.accept_flow_completion("run", "flow-a", &first).unwrap());
    let second = db.begin_flow_attempt("run", "flow-a", "new-flow").unwrap();
    assert_ne!(first, second);
    assert!(!db.accept_flow_completion("run", "flow-a", &first).unwrap());
    let old_stage = db.load_stage("stage").unwrap().unwrap();
    assert!(db.save_stage(&old_stage).is_err());
    let mut run = db.load_run("run").unwrap().unwrap();
    run.set_status(RunStatus::Cancelled);
    db.save_run(&run).unwrap();
    assert!(!db.accept_flow_completion("run", "flow-a", &second).unwrap());
}

#[test]
fn failed_completion_audit_does_not_consume_delivery() {
    let db = fixture();
    let attempt = db.begin_flow_attempt("run", "flow-a", "flow").unwrap();
    lock_conn!(db).execute_batch("CREATE TRIGGER reject_events BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT, 'injected audit failure'); END;").unwrap();
    assert!(
        db.accept_flow_completion("run", "flow-a", &attempt)
            .is_err()
    );
    lock_conn!(db)
        .execute_batch("DROP TRIGGER reject_events;")
        .unwrap();
    assert!(
        db.accept_flow_completion("run", "flow-a", &attempt)
            .unwrap()
    );
}

#[test]
fn failed_attempt_reservation_rolls_back_pointer_and_superseded_children() {
    let db = fixture();
    lock_conn!(db).execute_batch("CREATE TRIGGER reject_attempt BEFORE INSERT ON execution_attempts BEGIN SELECT RAISE(ABORT, 'injected attempt failure'); END;").unwrap();
    let before = db.list_events("run").unwrap().len();
    assert!(db.begin_flow_attempt("run", "flow-b", "new-flow").is_err());
    assert!(
        db.load_run("run")
            .unwrap()
            .unwrap()
            .current_flow_id()
            .is_none()
    );
    assert_eq!(
        *db.load_stage("stage").unwrap().unwrap().status(),
        StageStatus::Running
    );
    assert_eq!(
        *db.load_flow("flow").unwrap().unwrap().status(),
        FlowStatus::Running
    );
    assert_eq!(db.list_events("run").unwrap().len(), before);
}

#[test]
fn restart_fences_old_attempt_before_allowing_replacement() {
    let db = fixture();
    let old = db.begin_flow_attempt("run", "flow-a", "flow").unwrap();
    db.abandon_execution_on_restart("run").unwrap();
    let new = db
        .begin_flow_attempt("run", "flow-a", "replacement")
        .unwrap();
    assert!(!db.accept_flow_completion("run", "flow-a", &old).unwrap());
    assert!(db.accept_flow_completion("run", "flow-a", &new).unwrap());
    assert_eq!(
        *db.load_stage("stage").unwrap().unwrap().status(),
        StageStatus::Failed
    );
}

#[test]
fn saves_preserve_insertion_order_and_cannot_replace_existing_ids() {
    let db = fixture();
    db.save_flow(&FlowRecord::new(
        "second".into(),
        RunId("run".into()),
        "flow-a".into(),
        None,
        None,
        None,
    ))
    .unwrap();
    let first = db.load_flow("flow").unwrap().unwrap();
    db.save_flow(&first).unwrap();
    let ids: Vec<_> = db
        .load_flows_for_run("run")
        .unwrap()
        .into_iter()
        .map(|f| f.id().to_string())
        .collect();
    assert_eq!(ids, ["flow", "second"]);
    let replacement = FlowRecord::new(
        "flow".into(),
        RunId("run".into()),
        "other".into(),
        None,
        None,
        None,
    );
    assert!(db.save_flow(&replacement).is_err());
}

#[test]
fn deadline_is_immutable_and_expired_runs_cannot_start_work() {
    let db = fixture();
    let mut run = PipelineRun::new(
        "expired".into(),
        None,
        None,
        "deadline".into(),
        RunStatus::Running,
        None,
        None,
        None,
        None,
    );
    let deadline = (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
    run.set_deadline_at(Some(deadline.clone()));
    db.save_run(&run).unwrap();
    assert!(
        db.begin_flow_attempt("expired", "flow-a", "new-flow")
            .is_err()
    );
    let flow = FlowRecord::new(
        "expired-flow".into(),
        RunId("expired".into()),
        "flow-a".into(),
        None,
        None,
        None,
    );
    assert!(db.save_flow(&flow).is_err());
    run.set_deadline_at(Some(
        (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
    ));
    assert!(db.save_run(&run).is_err());
    assert_eq!(
        db.load_run("expired").unwrap().unwrap().deadline_at(),
        Some(deadline.as_str())
    );
}

#[test]
fn deadline_expiry_fences_late_stage_results_and_completion_delivery() {
    let db = fixture();
    let attempt = db.begin_flow_attempt("run", "flow-a", "flow").unwrap();
    let mut late_stage = db.load_stage("stage").unwrap().unwrap();
    // Inject an elapsed deadline to model the clock crossing it while work is
    // in flight, without relying on a sleep or a wall-clock scheduling race.
    lock_conn!(db)
        .execute(
            "UPDATE pipeline_runs SET deadline_at = '2000-01-01T00:00:00Z' WHERE id = 'run'",
            [],
        )
        .unwrap();
    late_stage.set_status(StageStatus::Completed);
    assert!(db.save_stage(&late_stage).is_err());
    assert!(
        !db.accept_flow_completion("run", "flow-a", &attempt)
            .unwrap()
    );
}

#[test]
fn separate_connections_compare_and_set_with_one_winner() {
    let path = std::env::temp_dir().join(format!("tj-cas-{}.db", uuid::Uuid::new_v4()));
    let db = Database::open(path.to_str().unwrap()).unwrap();
    seed(&db);
    let second = Database::open(path.to_str().unwrap()).unwrap();
    let a = db.load_stage("stage").unwrap().unwrap();
    let b = second.load_stage("stage").unwrap().unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = [
        (db, a, StageStatus::Completed),
        (second, b, StageStatus::Failed),
    ]
    .into_iter()
    .map(|(db, mut row, target)| {
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            row.set_status(target);
            barrier.wait();
            db.save_stage(&row).map_err(|error| error.to_string())
        })
    })
    .collect();
    let results: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert!(
        results
            .iter()
            .find_map(|result| result.as_ref().err())
            .unwrap()
            .contains("Lifecycle conflict")
    );
    let db = Database::open(path.to_str().unwrap()).unwrap();
    assert_eq!(db.list_events("run").unwrap().len(), 4);
    drop(db);
    std::fs::remove_file(path).unwrap();
}
