//! T-5 review fix: timeout semantics for the interactive tool loop.
//!
//! The pre-fix `execute` wrapped the WHOLE `run_tool_loop` in one
//! `tokio::time::timeout(timeout_secs)` (~300s from the ADCs), so the
//! blocking user wait (`question` → channel) raced a wall-clock deadline: an
//! answer after the budget dropped the future mid-receive and failed the
//! stage. The fix bounds each LLM call individually instead (interactive),
//! keeps the whole-loop deadline for non-interactive stages byte-identically,
//! and gives `send_followup` the same semantics. Sessions are registered
//! before the loop starts, so a timeout no longer erases the conversation.

mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::{Content, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part};
use async_trait::async_trait;
use serde_json::{Value, json};
use tj_runner::adk_runner::{ADKRunner, QUESTION_TOOL_NAME, VERDICT_TOOL_NAME};
use tj_runner::types::{
    QuestionAnswer, QuestionChannel, QuestionRequest, Runner, StageResult, StageStatus,
};

/// Sequential response mock whose script, once exhausted, NEVER resolves —
/// exactly the "LLM call genuinely hangs" shape the timeout budgets exist
/// for.
struct ScriptedLlm {
    name: String,
    responses: Mutex<VecDeque<LlmResponse>>,
}

impl ScriptedLlm {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            responses: Mutex::new(VecDeque::new()),
        }
    }

    fn with_response(self, response: LlmResponse) -> Self {
        self.responses.lock().unwrap().push_back(response);
        self
    }
}

#[async_trait]
impl Llm for ScriptedLlm {
    fn name(&self) -> &str {
        &self.name
    }

    async fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        let next = self.responses.lock().unwrap().pop_front();
        match next {
            Some(response) => {
                let stream = async_stream::stream! {
                    yield Ok(response);
                };
                Ok(Box::pin(stream))
            }
            // Script exhausted: hang forever (per-call budget must fire).
            None => std::future::pending::<adk_core::Result<LlmResponseStream>>().await,
        }
    }
}

fn tool_call_response(tool_name: &str, args: Value) -> LlmResponse {
    LlmResponse::new(Content {
        role: "model".to_string(),
        parts: vec![Part::FunctionCall {
            name: tool_name.to_string(),
            args,
            id: Some("call_test_123".to_string()),
            thought_signature: None,
        }],
    })
}

fn text_response(text: &str) -> LlmResponse {
    LlmResponse::new(Content::new("model").with_text(text))
}

/// Question channel whose user "answers" only after `delay`. With the pre-fix
/// whole-loop wrapper this delay competing against `timeout_secs` dropped the
/// future mid-receive; with per-call budgeting it must sail through.
struct DelayedChannel {
    delay: Duration,
}

#[async_trait]
impl QuestionChannel for DelayedChannel {
    async fn ask(
        &self,
        _stage_id: &str,
        _stage_name: &str,
        _request: &QuestionRequest,
    ) -> anyhow::Result<QuestionAnswer> {
        tokio::time::sleep(self.delay).await;
        Ok(QuestionAnswer {
            selected: vec!["sqlite".to_string()],
            text: Some("user note".to_string()),
        })
    }
}

fn question_args() -> Value {
    json!({
        "question": "Which storage engine?",
        "options": [{"id": "sqlite", "label": "SQLite"}, {"id": "postgres", "label": "Postgres"}],
        "multi_select": false,
        "free_form": true
    })
}

fn verdict_args() -> Value {
    json!({"verdict": "success", "content": "Done with the user's answer.", "summary": "ok"})
}

/// Extract the session id from a failed-but-addressable interactive result.
fn addressable_session_id(result: &StageResult) -> String {
    match &result.status {
        StageStatus::Failed { reason } => {
            assert!(reason.contains("output contract violation"));
            result
                .connection
                .as_ref()
                .and_then(|c| c.session_id.clone())
                .expect("interactive failure keeps the session addressable")
        }
        other => panic!("expected an addressable failure, got {:?}", other),
    }
}

#[tokio::test]
async fn answers_after_the_whole_loop_budget_still_succeed() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    // timeout_secs = 1, but BOTH user answers arrive after 1.2s each. The
    // pre-fix whole-loop wrapper would drop the future mid-receive at 1s and
    // fail the stage; per-call budgeting gives each ask its own fresh wait
    // (no shared wall-clock budget across questions).
    let mock = Arc::new(
        ScriptedLlm::new("test-model")
            .with_response(tool_call_response(QUESTION_TOOL_NAME, question_args()))
            .with_response(tool_call_response(QUESTION_TOOL_NAME, question_args()))
            .with_response(tool_call_response(VERDICT_TOOL_NAME, verdict_args())),
    );

    let runner = ADKRunner::new(Arc::clone(&mock) as Arc<dyn Llm>).with_question_channel(Arc::new(
        DelayedChannel {
            delay: Duration::from_millis(1200),
        },
    ));
    let mut task = common::make_task_definition(workspace, true);
    task.timeout_secs = 1;

    let result = runner.execute(task).await.unwrap();
    let output = common::assert_success(result);
    assert_eq!(output["verdict"], "success");
    assert_eq!(output["content"], "Done with the user's answer.");
}

#[tokio::test]
async fn non_interactive_llm_hang_still_times_out_as_a_whole() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    // Regression guard for the non-interactive channel: the whole-loop
    // deadline is untouched, so a genuinely hanging LLM call still yields
    // StageStatus::Timeout with the same error text as before.
    let mock = Arc::new(ScriptedLlm::new("test-model"));

    let runner = ADKRunner::new(Arc::clone(&mock) as Arc<dyn Llm>);
    let mut task = common::make_task_definition(workspace, false);
    task.timeout_secs = 1;

    let result = runner.execute(task).await.unwrap();
    assert!(matches!(result.status, StageStatus::Timeout));
    assert_eq!(result.error.as_deref(), Some("Timed out after 1s"));
}

#[tokio::test]
async fn interactive_llm_hang_hits_per_call_budget_and_session_survives() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    // Interactive counterpart: the per-call budget fires on the hanging LLM
    // call — the stage fails visibly — and the session (registered before the
    // loop started) stays in the map instead of being erased by the timeout.
    let mock = Arc::new(ScriptedLlm::new("test-model"));

    let runner = ADKRunner::new(Arc::clone(&mock) as Arc<dyn Llm>).with_question_channel(Arc::new(
        DelayedChannel {
            delay: Duration::from_millis(1200),
        },
    ));
    let mut task = common::make_task_definition(workspace, true);
    task.timeout_secs = 1;

    let result = runner.execute(task).await.unwrap();
    match &result.status {
        StageStatus::Failed { reason } => {
            assert!(
                reason.contains("LLM call timed out after 1s"),
                "reason '{}' lacks the per-call timeout",
                reason
            );
        }
        other => panic!("expected a per-call timeout failure, got {:?}", other),
    }
    assert_eq!(
        runner.session_count(),
        1,
        "session must survive the timeout"
    );
}

#[tokio::test]
async fn followup_user_wait_exceeds_the_budget_and_still_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    // send_followup parity: the followup turn's blocking ask also sits
    // outside any wall-clock deadline. First execution exhausts the
    // silent-turn nudge bound (nudge budget 1 → two prose turns) to keep the
    // session addressable; the followup then asks (1.2s answer, budget 1s)
    // and completes via verdict.
    let mock = Arc::new(
        ScriptedLlm::new("test-model")
            .with_response(text_response("Prose turn one."))
            .with_response(text_response("Prose turn two."))
            .with_response(tool_call_response(QUESTION_TOOL_NAME, question_args()))
            .with_response(tool_call_response(VERDICT_TOOL_NAME, verdict_args())),
    );

    let runner = ADKRunner::new(Arc::clone(&mock) as Arc<dyn Llm>).with_question_channel(Arc::new(
        DelayedChannel {
            delay: Duration::from_millis(1200),
        },
    ));
    let mut task = common::make_task_definition(workspace, true);
    task.timeout_secs = 1;
    task.adc.max_output_retries = 1;

    let first = runner.execute(task).await.unwrap();
    let session_id = addressable_session_id(&first);

    let second = runner
        .send_followup(&session_id, "here is my answer")
        .await
        .unwrap();
    let output = common::assert_success(second);
    assert_eq!(output["verdict"], "success");
}

#[tokio::test]
async fn followup_llm_hang_is_bounded_per_call() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    // send_followup parity, hang shape: the followup turn's LLM call genuinely
    // hangs, so the per-call budget must fire — the pre-fix path had NO
    // timeout at all and would hang forever.
    let mock = Arc::new(
        ScriptedLlm::new("test-model")
            .with_response(text_response("Prose turn one."))
            .with_response(text_response("Prose turn two.")),
    );

    let runner = ADKRunner::new(Arc::clone(&mock) as Arc<dyn Llm>).with_question_channel(Arc::new(
        DelayedChannel {
            delay: Duration::from_millis(1200),
        },
    ));
    let mut task = common::make_task_definition(workspace, true);
    task.timeout_secs = 1;
    task.adc.max_output_retries = 1;

    let first = runner.execute(task).await.unwrap();
    let session_id = addressable_session_id(&first);

    let error = runner
        .send_followup(&session_id, "here is my answer")
        .await
        .expect_err("hanging followup LLM call must hit the per-call budget");
    assert!(
        error.to_string().contains("LLM call timed out after 1s"),
        "error '{}' lacks the per-call timeout",
        error
    );
}
