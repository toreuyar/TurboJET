use super::*;
use std::sync::{Arc, Barrier};

fn test_db() -> (Arc<Database>, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("tj-db-questions-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Arc::new(Database::open(&dir.join("state.db").to_string_lossy()).unwrap());
    (db, dir)
}

fn seed_stage(db: &Database, run_id: &str, flow_id: &str, stage_id: &str, iteration: u32) {
    let mut run = PipelineRun::new(
        run_id.into(),
        None,
        None,
        "question test".into(),
        RunStatus::Running,
        None,
        None,
        None,
        None,
    );
    if run_id == "run" {
        run.set_workflow_id(None);
    }
    if db.load_run(run_id).unwrap().is_none() {
        db.save_run(&run).unwrap();
    }
    if db.load_flow(flow_id).unwrap().is_none() {
        db.save_flow(&FlowRecord::new(
            flow_id.into(),
            RunId(run_id.into()),
            "flow-a".into(),
            Some(1),
            None,
            None,
        ))
        .unwrap();
    }
    if db.load_stage(stage_id).unwrap().is_none() {
        let mut stage = StageRecord::new(
            stage_id.into(),
            FlowId(flow_id.into()),
            "stage-a".into(),
            StageStatus::Running,
            iteration,
            None,
        );
        db.save_stage(&stage).unwrap();
        stage.set_status(StageStatus::WaitingForInput);
        db.save_stage(&stage).unwrap();
    }
}

fn question(
    db: &Database,
    run_id: &str,
    flow_id: &str,
    stage_id: &str,
    attempt: u32,
) -> QuestionRecord {
    let q = QuestionRecord::new(
        format!("question_{}", uuid::Uuid::new_v4()),
        RunId(run_id.into()),
        flow_id.into(),
        stage_id.into(),
        "stage-a".into(),
        "flow-a".into(),
        attempt,
        r#"{"question":"Which database?","options":[{"id":"postgres","label":"PostgreSQL"}],"multi_select":false,"free_form":true}"#.into(),
    );
    db.create_question(&q).unwrap();
    q
}

#[test]
fn one_open_question_accepts_and_replays_idempotently() {
    let (db, dir) = test_db();
    seed_stage(&db, "run", "flow", "stage", 0);
    let q = question(&db, "run", "flow", "stage", 0);

    let accepted = db
        .answer_question(
            &q.id,
            "run",
            "stage",
            Some(0),
            r#"{"selected":["postgres"]}"#,
            "webui",
            "key-1",
        )
        .unwrap();
    assert!(!accepted.replayed);
    assert_eq!(accepted.question.status, QuestionStatus::Answered);
    assert_eq!(accepted.question.answer_source.as_deref(), Some("webui"));

    let replay = db
        .answer_question(
            &q.id,
            "run",
            "stage",
            Some(0),
            r#"{"selected":["postgres"]}"#,
            "webui",
            "key-1",
        )
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.question.answer, accepted.question.answer);

    let conflict = db
        .answer_question(
            &q.id,
            "run",
            "stage",
            Some(0),
            r#"{"selected":["sqlite"]}"#,
            "webui",
            "key-2",
        )
        .unwrap_err()
        .to_string();
    assert!(conflict.contains("already answered"), "{conflict}");
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn cross_stage_cross_run_attempt_and_closed_replies_conflict() {
    let (db, dir) = test_db();
    seed_stage(&db, "run", "flow", "stage", 1);
    let q = question(&db, "run", "flow", "stage", 1);

    let wrong_stage = db
        .answer_question(
            &q.id,
            "run",
            "stage-other",
            Some(1),
            "answer",
            "webui",
            "k-stage",
        )
        .unwrap_err()
        .to_string();
    assert!(wrong_stage.contains("cross-stage"), "{wrong_stage}");

    let wrong_run = db
        .answer_question(
            &q.id,
            "run-other",
            "stage",
            Some(1),
            "answer",
            "webui",
            "k-run",
        )
        .unwrap_err()
        .to_string();
    assert!(wrong_run.contains("belongs to run"), "{wrong_run}");

    let wrong_attempt = db
        .answer_question(
            &q.id,
            "run",
            "stage",
            Some(0),
            "answer",
            "webui",
            "k-attempt",
        )
        .unwrap_err()
        .to_string();
    assert!(wrong_attempt.contains("stale attempt"), "{wrong_attempt}");

    assert!(
        db.close_question(&q.id, QuestionStatus::Cancelled, "test closed")
            .unwrap()
    );
    let late = db
        .answer_question(&q.id, "run", "stage", Some(1), "answer", "zulip", "k-late")
        .unwrap_err()
        .to_string();
    assert!(late.contains("cancelled"), "{late}");
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn simultaneous_transports_accept_exactly_one_answer() {
    let (db, dir) = test_db();
    seed_stage(&db, "run", "flow", "stage", 0);
    let q = question(&db, "run", "flow", "stage", 0);
    let barrier = Arc::new(Barrier::new(2));
    let db_path = dir.join("state.db").to_string_lossy().into_owned();

    let mut handles = Vec::new();
    for (source, key, answer) in [
        ("webui", "race-web", r#"{"selected":["postgres"]}"#),
        ("zulip", "race-zulip", r#"{"selected":["sqlite"]}"#),
    ] {
        let path = db_path.clone();
        let qid = q.id.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            // Separate connections contend on the same SQLite file, proving
            // conditional acceptance rather than in-process serialization.
            let db = Database::open(&path).unwrap();
            barrier.wait();
            db.answer_question(&qid, "run", "stage", Some(0), answer, source, key)
                .map(|acceptance| (source, acceptance))
        }));
    }
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let winners = results.iter().filter(|result| result.is_ok()).count();
    assert_eq!(winners, 1, "exactly one transport may accept: {results:?}");
    let stored = db.load_question(&q.id).unwrap().unwrap();
    assert_eq!(stored.status, QuestionStatus::Answered);
    assert_eq!(
        db.list_events("run")
            .unwrap()
            .iter()
            .filter(|e| e.kind == "question_answered")
            .count(),
        1
    );
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn terminal_run_closes_pending_questions_and_rejects_answers() {
    let (db, dir) = test_db();
    seed_stage(&db, "run", "flow", "stage", 0);
    let q = question(&db, "run", "flow", "stage", 0);

    let mut run = db.load_run("run").unwrap().unwrap();
    run.set_status(RunStatus::Cancelled);
    db.save_run(&run).unwrap();
    assert_eq!(db.list_questions_for_run("run", true).unwrap().len(), 0);
    let err = db
        .answer_question(
            &q.id,
            "run",
            "stage",
            Some(0),
            "late answer",
            "webui",
            "terminal-key",
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("cancelled"), "{err}");
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn pending_questions_are_bounded_per_run() {
    let (db, dir) = test_db();
    seed_stage(&db, "run", "flow", "stage", 0);
    for index in 0..MAX_PENDING_QUESTIONS_PER_RUN {
        question(&db, "run", "flow", &format!("stage-{index}"), 0);
    }
    let extra = QuestionRecord::new(
        "question-over".into(),
        RunId("run".into()),
        "flow".into(),
        "stage-extra".into(),
        "stage-a".into(),
        "flow-a".into(),
        0,
        "{}".into(),
    );
    let error = db.create_question(&extra).unwrap_err().to_string();
    assert!(error.contains("pending questions"), "{error}");
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}
