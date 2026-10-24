//! T-5 acceptance tests: the stage-side [`QuestionChannel`] over a chat
//! session — rendered question delivery, durable status flips, answer
//! parsing (JSON / letters / labels / free text), stage-log round-trip,
//! channel-error propagation, and the resurrect-guard on terminal rows.

mod common;

use std::sync::Arc;

use tj_db::StageStatus;
use tj_runner::{QuestionChannel, QuestionRequest};
use tj_stage::{ChatQuestionChannel, parse_answer, render_question};

use common::{ScriptedSession, insert_running_stage, logs_for, sample_request, wait_for_status};

fn channel(
    db: &Arc<tj_db::Database>,
    session: &Arc<ScriptedSession>,
    run_id: &str,
    max_reasks: u32,
) -> ChatQuestionChannel {
    ChatQuestionChannel::new(
        Arc::clone(db),
        session.clone(),
        run_id.to_string(),
        "flow-a".to_string(),
        max_reasks,
    )
}

#[tokio::test]
async fn ask_delivers_question_flips_status_and_returns_json_answer() {
    let db = common::test_db();
    insert_running_stage(&db, "run_q", "flow_q", "stage_q1");
    let session = ScriptedSession::new();
    let channel = channel(&db, &session, "run_q", 0);
    let request = sample_request();

    let handle = tokio::spawn(async move { channel.ask("stage_q1", "stage-a", &request).await });

    // While blocked in receive(), the durable flip must be visible to the
    // manager/webui gates (send_web_input hard-gates on this status).
    wait_for_status(&db, "stage_q1", StageStatus::WaitingForInput).await;

    // Question delivered to the chat session in the OQ-3 plain-text form.
    let sent = session.sent();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].contains("**stage-a asks:**"), "got: {}", sent[0]);
    assert!(sent[0].contains("Which database should we use?"));
    assert!(sent[0].contains("  a) Option A"), "got: {}", sent[0]);
    assert!(sent[0].contains("Reply with letters, e.g. `a, c`"));

    // Stage logs: rendered text + structured payload written before the wait.
    let logs = logs_for(&db, "run_q");
    assert!(
        logs.iter()
            .any(|l| l.kind == "agent_message" && l.content.contains("**stage-a asks:**"))
    );
    let question_row = logs
        .iter()
        .find(|l| l.kind == "question")
        .expect("question row with structured payload");
    assert!(question_row.content.contains("\"multi_select\":false"));
    assert!(question_row.content.contains("opt-a"));
    assert_eq!(question_row.stage_id, "stage_q1");

    // The webui-style JSON answer flows back as the structured tool result.
    session.reply(r#"{"selected": ["opt-b"], "text": "postgres please"}"#);
    let answer = handle.await.unwrap().expect("ask succeeds");
    assert_eq!(answer.selected, vec!["opt-b".to_string()]);
    assert_eq!(answer.text.as_deref(), Some("postgres please"));

    // Durable flip restored; answer persisted as user_input + question_answer.
    let row = db.load_stage("stage_q1").unwrap().unwrap();
    assert_eq!(*row.status(), StageStatus::Running);
    let logs = logs_for(&db, "run_q");
    assert!(
        logs.iter()
            .any(|l| l.kind == "user_input" && l.content.contains("postgres please"))
    );
    let answer_row = logs
        .iter()
        .find(|l| l.kind == "question_answer")
        .expect("question_answer row");
    assert!(answer_row.content.contains("\"selected\":[\"opt-b\"]"));
}

#[tokio::test]
async fn letter_label_and_number_replies_map_to_option_ids() {
    let db = common::test_db();
    insert_running_stage(&db, "run_l", "flow_l", "stage_l1");
    let session = ScriptedSession::new();
    let channel = channel(&db, &session, "run_l", 0);
    let request = sample_request();

    let handle = tokio::spawn(async move { channel.ask("stage_l1", "stage-a", &request).await });
    wait_for_status(&db, "stage_l1", StageStatus::WaitingForInput).await;

    // Zulip-style lettered reply with notes after the separator (OQ-3).
    session.reply("a, c — going with both because x");
    let answer = handle.await.unwrap().expect("ask succeeds");
    assert_eq!(
        answer.selected,
        vec!["opt-a".to_string(), "opt-c".to_string()]
    );
    assert_eq!(answer.text.as_deref(), Some("going with both because x"));
}

#[tokio::test]
async fn channel_error_restores_running_and_propagates() {
    let db = common::test_db();
    insert_running_stage(&db, "run_e", "flow_e", "stage_e1");
    let session = ScriptedSession::new();
    session.fail_receive(true);
    let channel = channel(&db, &session, "run_e", 0);
    let request = sample_request();

    let result = channel.ask("stage_e1", "stage-a", &request).await;
    assert!(result.is_err(), "channel failure must propagate");

    // On channel error the stage is restored to Running (best effort).
    let row = db.load_stage("stage_e1").unwrap().unwrap();
    assert_eq!(*row.status(), StageStatus::Running);
    // The question was still asked and logged; no answer rows exist.
    let logs = logs_for(&db, "run_e");
    assert!(logs.iter().any(|l| l.kind == "question"));
    assert!(!logs.iter().any(|l| l.kind == "user_input"));
    assert!(!logs.iter().any(|l| l.kind == "question_answer"));
}

#[tokio::test]
async fn resurrect_guard_aborts_restore_when_row_went_terminal_mid_ask() {
    let db = common::test_db();
    insert_running_stage(&db, "run_r", "flow_r", "stage_r1");
    let session = ScriptedSession::new();
    let channel = channel(&db, &session, "run_r", 0);
    let request = sample_request();

    let handle = tokio::spawn(async move { channel.ask("stage_r1", "stage-a", &request).await });
    wait_for_status(&db, "stage_r1", StageStatus::WaitingForInput).await;

    // Simulate the watchdog finalize mid-ask — the exact write
    // finalize_stale_waiting_stages performs on a waiting_for_input row
    // when the run is torn down.
    let mut row = db.load_stage("stage_r1").unwrap().unwrap();
    row.set_status(StageStatus::Failed);
    row.set_error(Some(
        "run ended while the stage was waiting for input".into(),
    ));
    row.set_completed_at(Some(chrono::Utc::now().to_rfc3339()));
    db.save_stage(&row).unwrap();

    // The user answers anyway (e.g. a stale Zulip reply arrives late).
    session.reply("a");
    let result = handle.await.unwrap();
    let error = result.expect_err("ask must abort, not resurrect the terminal row");
    assert!(error.to_string().contains("terminal"), "got: {}", error);

    // The terminal row is left exactly as the watchdog wrote it.
    let row = db.load_stage("stage_r1").unwrap().unwrap();
    assert_eq!(*row.status(), StageStatus::Failed);
    assert_eq!(
        row.error(),
        Some("run ended while the stage was waiting for input")
    );
    // No answer was consumed into a resurrected record.
    let logs = logs_for(&db, "run_r");
    assert!(!logs.iter().any(|l| l.kind == "question_answer"));
}

#[tokio::test]
async fn guard_refuses_waiting_flip_on_already_terminal_row() {
    let db = common::test_db();
    insert_running_stage(&db, "run_g", "flow_g", "stage_g1");
    // Row already terminal before the ask starts.
    let mut row = db.load_stage("stage_g1").unwrap().unwrap();
    row.set_status(StageStatus::Failed);
    row.set_error(Some("killed earlier".into()));
    db.save_stage(&row).unwrap();

    let session = ScriptedSession::new();
    let channel = channel(&db, &session, "run_g", 0);
    let request = sample_request();

    let result = channel.ask("stage_g1", "stage-a", &request).await;
    let error = result.expect_err("guard must abort the ask");
    assert!(error.to_string().contains("terminal"), "got: {}", error);

    let row = db.load_stage("stage_g1").unwrap().unwrap();
    assert_eq!(*row.status(), StageStatus::Failed);
    assert_eq!(row.error(), Some("killed earlier"));
    // The ask never blocked: no reply was consumed.
    assert!(
        session.sent().is_empty()
            || !logs_for(&db, "run_g")
                .iter()
                .any(|l| l.kind == "question_answer")
    );
}

// ---------------------------------------------------------------------------
// Pure parser arms (the Zulip-parity reply parser; T-12 hardens further).
// ---------------------------------------------------------------------------

#[test]
fn parser_accepts_json_object_answer_with_label_fallback() {
    let request = sample_request();
    // Label text instead of ids is accepted case-insensitively (OQ-2a).
    let answer = parse_answer(
        r#"{"selected": ["OPTION C", "opt-a"], "text": "both"}"#,
        &request,
    );
    assert_eq!(
        answer.selected,
        vec!["opt-c".to_string(), "opt-a".to_string()]
    );
    assert_eq!(answer.text.as_deref(), Some("both"));
}

#[test]
fn parser_maps_numbers_letters_and_markdown_ticks() {
    let request = sample_request();
    let answer = parse_answer("`b`", &request);
    assert_eq!(answer.selected, vec!["opt-b".to_string()]);
    let answer = parse_answer("2 and 3", &request);
    assert_eq!(
        answer.selected,
        vec!["opt-b".to_string(), "opt-c".to_string()]
    );
    let answer = parse_answer("A.", &request);
    assert_eq!(answer.selected, vec!["opt-a".to_string()]);
}

#[test]
fn parser_keeps_unmatched_tokens_as_free_text() {
    let request = sample_request();
    let answer = parse_answer("b, but faster please", &request);
    assert_eq!(answer.selected, vec!["opt-b".to_string()]);
    assert_eq!(answer.text.as_deref(), Some("but faster please"));
}

#[test]
fn parser_free_text_only_question_takes_whole_reply() {
    let request = QuestionRequest {
        question: "Any constraints?".to_string(),
        options: vec![],
        multi_select: false,
        free_form: true,
    };
    let answer = parse_answer("must stay on sqlite", &request);
    assert!(answer.selected.is_empty());
    assert_eq!(answer.text.as_deref(), Some("must stay on sqlite"));
}

#[test]
fn parser_json_scalar_falls_through_to_free_text_parsing() {
    let request = sample_request();
    // A bare JSON scalar must NOT be read as an answer object (OQ-2b);
    // it falls through to the letter/free-text parser, where it matches no
    // option and becomes free text.
    let answer = parse_answer(r#""teal""#, &request);
    assert!(answer.selected.is_empty());
    assert_eq!(answer.text.as_deref(), Some(r#""teal""#));
}

#[test]
fn renderer_hints_match_round_shape() {
    let multi_free = QuestionRequest {
        question: "q".into(),
        options: vec![
            tj_runner::QuestionOption {
                id: "x".into(),
                label: "X".into(),
            },
            tj_runner::QuestionOption {
                id: "y".into(),
                label: "Y".into(),
            },
        ],
        multi_select: true,
        free_form: true,
    };
    let text = render_question("stage-a", &multi_free);
    assert!(
        text.contains("(select all that apply; free text welcome)"),
        "got: {}",
        text
    );

    let free_only = QuestionRequest {
        question: "q".into(),
        options: vec![],
        multi_select: false,
        free_form: true,
    };
    let text = render_question("stage-a", &free_only);
    assert!(text.contains("(free text)"), "got: {}", text);
    assert!(text.contains("Reply with your answer."));
    assert!(!text.contains("::TURBOJET:DONE::"));
}

// ---------------------------------------------------------------------------
// T-12: messy-but-legitimate Zulip reply shapes + parse precedence.
// ---------------------------------------------------------------------------

#[test]
fn parser_handles_mixed_case_and_and_whitespace_newline_lists() {
    let request = sample_request();
    let answer = parse_answer("A, C", &request);
    assert_eq!(
        answer.selected,
        vec!["opt-a".to_string(), "opt-c".to_string()]
    );
    let answer = parse_answer("B AND c", &request);
    assert_eq!(
        answer.selected,
        vec!["opt-b".to_string(), "opt-c".to_string()]
    );
    // Newline-separated answers (multi-line Zulip replies).
    let answer = parse_answer("a\nc\n", &request);
    assert_eq!(
        answer.selected,
        vec!["opt-a".to_string(), "opt-c".to_string()]
    );
}

#[test]
fn parser_handles_wrapping_and_trailing_punctuation() {
    let request = sample_request();
    assert_eq!(
        parse_answer("a.", &request).selected,
        vec!["opt-a".to_string()]
    );
    assert_eq!(
        parse_answer("(b)", &request).selected,
        vec!["opt-b".to_string()]
    );
    assert_eq!(
        parse_answer("a)", &request).selected,
        vec!["opt-a".to_string()]
    );
    assert_eq!(
        parse_answer("`c`.", &request).selected,
        vec!["opt-c".to_string()]
    );
    assert_eq!(
        parse_answer("\"a\"", &request).selected,
        vec!["opt-a".to_string()]
    );
    assert_eq!(
        parse_answer("2.", &request).selected,
        vec!["opt-b".to_string()]
    );
}

#[test]
fn parser_precedence_id_beats_letter_beats_label() {
    // Exact option-id match beats the positional letter map: `b` is an id
    // (first option), NOT the letter that would point at the second option.
    let request = QuestionRequest {
        question: "pick".to_string(),
        options: vec![
            tj_runner::QuestionOption {
                id: "b".to_string(),
                label: "Alpha".to_string(),
            },
            tj_runner::QuestionOption {
                id: "z".to_string(),
                label: "Beta".to_string(),
            },
        ],
        multi_select: false,
        free_form: false,
    };
    assert_eq!(parse_answer("b", &request).selected, vec!["b".to_string()]);

    // Positional letter beats label: `b` is the letter for the SECOND option
    // even though the first option's label is also "b" — the letter map wins
    // the "label 'a' vs letter 'a'" dispute, by documented precedence.
    let request = QuestionRequest {
        question: "pick".to_string(),
        options: vec![
            tj_runner::QuestionOption {
                id: "x".to_string(),
                label: "b".to_string(),
            },
            tj_runner::QuestionOption {
                id: "y".to_string(),
                label: "c".to_string(),
            },
        ],
        multi_select: false,
        free_form: false,
    };
    assert_eq!(parse_answer("b", &request).selected, vec!["y".to_string()]);
    // The digit map also beats a colliding label: label "2" on the first
    // option loses to the 1-based digit `2` (second option).
    assert_eq!(parse_answer("2", &request).selected, vec!["y".to_string()]);
}

#[test]
fn parser_combined_multi_select_with_free_form_note() {
    let request = sample_request();
    let answer = parse_answer("a, c — plus check the logs", &request);
    assert_eq!(
        answer.selected,
        vec!["opt-a".to_string(), "opt-c".to_string()]
    );
    assert_eq!(answer.text.as_deref(), Some("plus check the logs"));
    // Colon separator and "and" join in one reply.
    let answer = parse_answer("a and c: plus check the logs", &request);
    assert_eq!(
        answer.selected,
        vec!["opt-a".to_string(), "opt-c".to_string()]
    );
    assert_eq!(answer.text.as_deref(), Some("plus check the logs"));
}

#[test]
fn renderer_output_survives_zulip_markdown() {
    let text = render_question("stage-a", &sample_request());
    for line in text.lines() {
        let indent = line.len() - line.trim_start().len();
        assert!(
            indent < 4,
            "4+ space indent renders as a Zulip code block: {line:?}"
        );
        assert!(
            !line.trim_start().starts_with("1."),
            "an `N.` marker would be auto-renumbered as a Zulip ordered list: {line:?}"
        );
    }
    assert!(text.contains("**stage-a asks:**"), "got: {}", text);
    assert!(text.contains("  a) Option A"), "got: {}", text);
    assert!(
        text.contains("Reply with letters, e.g. `a, c`"),
        "got: {}",
        text
    );
}

// ---------------------------------------------------------------------------
// T-12: bounded re-ask loop over the scripted chat session.
// ---------------------------------------------------------------------------

/// The re-ask path only exists for letters-only questions (`free_form: false`)
/// — with free form allowed, any prose is already a valid answer.
fn letters_only_request() -> QuestionRequest {
    let mut request = sample_request();
    request.free_form = false;
    request
}

#[tokio::test]
async fn unparseable_reply_reasks_up_to_bound_then_errors_visibly() {
    let db = common::test_db();
    insert_running_stage(&db, "run_k", "flow_k", "stage_k1");
    let session = ScriptedSession::new();
    let channel = channel(&db, &session, "run_k", 2);
    let request = letters_only_request();

    let handle = tokio::spawn(async move { channel.ask("stage_k1", "stage-a", &request).await });
    wait_for_status(&db, "stage_k1", StageStatus::WaitingForInput).await;

    // Three consecutive unusable replies against a bound of 2.
    session.reply("what??");
    session.reply("hmm no idea");
    session.reply("still nothing");

    let result = handle.await.unwrap();
    let error = result.expect_err("bound exhausted → the ask errors visibly");
    let message = error.to_string();
    assert!(message.contains("could not be parsed"), "got: {message}");
    assert!(message.contains("after 2 re-ask(s)"), "got: {message}");
    assert!(
        message.contains("still nothing"),
        "error names the last reply: {message}"
    );

    // 1 question + 2 clarifications sent; every bad reply logged as user_input.
    let sent = session.sent();
    assert_eq!(sent.len(), 3);
    assert!(sent[1].contains("couldn't parse"), "got: {}", sent[1]);
    assert!(sent[2].contains("couldn't parse"), "got: {}", sent[2]);
    let logs = logs_for(&db, "run_k");
    assert_eq!(logs.iter().filter(|l| l.kind == "user_input").count(), 3);
    assert_eq!(logs.iter().filter(|l| l.kind == "agent_message").count(), 3);
    assert!(!logs.iter().any(|l| l.kind == "question_answer"));

    // The stage row was restored to Running (the failure surfaces via the
    // runner; the durable row must not stay WaitingForInput).
    let row = db.load_stage("stage_k1").unwrap().unwrap();
    assert_eq!(*row.status(), StageStatus::Running);
}

#[tokio::test]
async fn reask_recovers_when_user_answers_with_letters() {
    let db = common::test_db();
    insert_running_stage(&db, "run_s", "flow_s", "stage_s1");
    let session = ScriptedSession::new();
    let channel = channel(&db, &session, "run_s", 3);
    let request = letters_only_request();

    let handle = tokio::spawn(async move { channel.ask("stage_s1", "stage-a", &request).await });
    wait_for_status(&db, "stage_s1", StageStatus::WaitingForInput).await;

    session.reply("what??");
    session.reply("a, c");

    let answer = handle
        .await
        .unwrap()
        .expect("recovers after one clarification");
    assert_eq!(
        answer.selected,
        vec!["opt-a".to_string(), "opt-c".to_string()]
    );

    // Question + exactly one clarification; the accepted answer is logged.
    assert_eq!(session.sent().len(), 2);
    assert!(session.sent()[1].contains("couldn't parse"));
    let answer_row = logs_for(&db, "run_s")
        .into_iter()
        .find(|l| l.kind == "question_answer")
        .expect("question_answer row");
    assert!(
        answer_row
            .content
            .contains("\"selected\":[\"opt-a\",\"opt-c\"]")
    );
    let row = db.load_stage("stage_s1").unwrap().unwrap();
    assert_eq!(*row.status(), StageStatus::Running);
}

#[tokio::test]
async fn zero_bound_fails_on_first_unparseable_reply() {
    let db = common::test_db();
    insert_running_stage(&db, "run_z", "flow_z", "stage_z1");
    let session = ScriptedSession::new();
    let channel = channel(&db, &session, "run_z", 0);
    let request = letters_only_request();

    let handle = tokio::spawn(async move { channel.ask("stage_z1", "stage-a", &request).await });
    wait_for_status(&db, "stage_z1", StageStatus::WaitingForInput).await;

    session.reply("nope");
    let error = handle
        .await
        .unwrap()
        .expect_err("no re-asks allowed at bound 0");
    assert!(
        error.to_string().contains("after 0 re-ask(s)"),
        "got: {error}"
    );
    // Only the original question was sent — no clarification at bound 0.
    assert_eq!(session.sent().len(), 1);
}

#[tokio::test]
async fn free_form_question_accepts_prose_without_reask() {
    let db = common::test_db();
    insert_running_stage(&db, "run_p", "flow_p", "stage_p1");
    let session = ScriptedSession::new();
    // sample_request has options + free_form: true — prose is a valid answer.
    let channel = channel(&db, &session, "run_p", 2);
    let request = sample_request();

    let handle = tokio::spawn(async move { channel.ask("stage_p1", "stage-a", &request).await });
    wait_for_status(&db, "stage_p1", StageStatus::WaitingForInput).await;

    session.reply("please just pick postgres for us");
    let answer = handle
        .await
        .unwrap()
        .expect("prose accepted on a free_form question");
    assert!(answer.selected.is_empty());
    assert_eq!(
        answer.text.as_deref(),
        Some("please just pick postgres for us")
    );
    // No clarification was needed.
    assert_eq!(session.sent().len(), 1);
}

#[tokio::test]
async fn empty_reply_reasks_even_on_free_form_question() {
    let db = common::test_db();
    insert_running_stage(&db, "run_m", "flow_m", "stage_m1");
    let session = ScriptedSession::new();
    let channel = channel(&db, &session, "run_m", 1);
    let request = QuestionRequest {
        question: "Any constraints?".to_string(),
        options: vec![],
        multi_select: false,
        free_form: true,
    };

    let handle = tokio::spawn(async move { channel.ask("stage_m1", "stage-a", &request).await });
    wait_for_status(&db, "stage_m1", StageStatus::WaitingForInput).await;

    // An empty reply is never usable (plan T-12), free_form or not.
    session.reply("   ");
    session.reply("must stay on sqlite");

    let answer = handle
        .await
        .unwrap()
        .expect("recovers after the empty reply");
    assert!(answer.selected.is_empty());
    assert_eq!(answer.text.as_deref(), Some("must stay on sqlite"));
    assert_eq!(session.sent().len(), 2);
    assert!(
        session.sent()[1].contains("plain text"),
        "got: {}",
        session.sent()[1]
    );
}
