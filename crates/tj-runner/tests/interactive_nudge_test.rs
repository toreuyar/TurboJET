//! T-4 acceptance tests: the silent-turn nudge for interactive stages (FR-8)
//! — a turn that ends with no tool call never completes the stage; the runner
//! nudges the same session back into the `question`/`verdict` tool contract,
//! bounded by the ADC's `max_output_retries` (a counter separate from the
//! verdict/question poke budget); exhaustion fails visibly with the
//! `output contract violation:` prefix. Non-interactive final-text behavior
//! is regression-guarded.

mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use adk_core::{Content, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part};
use async_trait::async_trait;
use serde_json::{Value, json};
use tj_runner::adk_runner::{ADKRunner, VERDICT_TOOL_NAME};
use tj_runner::types::{Runner, StageStatus};

use common::RecordingSink;

/// Sequential response mock that records every `LlmRequest` so tests can
/// assert the nudge followup the model actually received.
struct RecordingMockLlm {
    name: String,
    responses: Mutex<VecDeque<LlmResponse>>,
    requests: Mutex<Vec<LlmRequest>>,
}

impl RecordingMockLlm {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            responses: Mutex::new(VecDeque::new()),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn with_response(self, response: LlmResponse) -> Self {
        self.responses.lock().unwrap().push_back(response);
        self
    }

    fn requests(&self) -> Vec<LlmRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl Llm for RecordingMockLlm {
    fn name(&self) -> &str {
        &self.name
    }

    async fn generate_content(
        &self,
        req: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        self.requests.lock().unwrap().push(req);
        let response = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("RecordingMockLlm ran out of responses");
        let stream = async_stream::stream! {
            yield Ok(response);
        };
        Ok(Box::pin(stream))
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

fn user_texts(request: &LlmRequest) -> Vec<String> {
    request
        .contents
        .iter()
        .filter(|content| content.role == "user")
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| part.text().map(str::to_string))
        .collect()
}

fn assert_contract_failure(result: &tj_runner::types::StageResult, expected_fragments: &[&str]) {
    match &result.status {
        StageStatus::Failed { reason } => {
            assert!(
                reason.starts_with("output contract violation:"),
                "failure reason must carry the machine-detectable prefix, got: {}",
                reason
            );
            for fragment in expected_fragments {
                assert!(
                    reason.contains(fragment),
                    "reason '{}' lacks '{}'",
                    reason,
                    fragment
                );
            }
        }
        other => panic!("expected a contract-violation failure, got {:?}", other),
    }
    assert!(
        !matches!(
            result.status,
            StageStatus::WaitingForInput | StageStatus::Success
        ),
        "contract failures must never succeed nor wait for input"
    );
}

#[tokio::test]
async fn silent_turn_then_verdict_call_recovers_and_completes() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    let mock = Arc::new(
        RecordingMockLlm::new("test-model")
            .with_response(text_response(
                "I collected the requirements, here is a long prose status update.",
            ))
            .with_response(tool_call_response(
                VERDICT_TOOL_NAME,
                json!({"verdict": "success", "content": "Requirements extracted.", "summary": "done"}),
            )),
    );

    let sink = Arc::new(RecordingSink::new());
    let runner = ADKRunner::new(Arc::clone(&mock) as Arc<dyn Llm>).with_sink(sink.clone());
    let task = common::make_task_definition(workspace, true);
    let result = runner.execute(task).await.unwrap();
    let output = common::assert_success(result);
    assert_eq!(output["verdict"], "success");

    // FR-23(c): the nudge row names the turn and the budget.
    let nudges = sink.by_kind("nudge");
    assert_eq!(nudges.len(), 1);
    assert!(
        nudges[0].contains("1/3"),
        "nudge row must name turn and count, got: {}",
        nudges[0]
    );

    // The corrective followup is recorded as an llm_request row.
    let corrective: Vec<String> = sink
        .by_kind("llm_request")
        .into_iter()
        .filter(|content| content.contains("Your turn ended without a tool call"))
        .collect();
    assert_eq!(
        corrective.len(),
        1,
        "exactly one nudge followup: {:?}",
        corrective
    );
    assert!(
        corrective[0].contains("Nudge 1/3"),
        "got: {}",
        corrective[0]
    );

    // The model actually received the nudge as a user turn in the SAME
    // conversation before its recovering verdict call.
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        user_texts(&requests[1])
            .iter()
            .any(|text| text.contains("Your turn ended without a tool call")),
        "the nudge must reach the model as a user turn"
    );
}

#[tokio::test]
async fn repeated_silent_turns_exhaust_bound_then_fail_visibly() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    // Default bound 3: silent turns 1-3 are nudged, turn 4 fails the stage.
    let mut mock = RecordingMockLlm::new("test-model");
    for _ in 0..5 {
        mock = mock.with_response(text_response("Quietly continuing in prose."));
    }
    let mock = Arc::new(mock);

    let sink = Arc::new(RecordingSink::new());
    let runner = ADKRunner::new(Arc::clone(&mock) as Arc<dyn Llm>).with_sink(sink.clone());
    let task = common::make_task_definition(workspace, true);
    let result = runner.execute(task).await.unwrap();

    assert_contract_failure(
        &result,
        &["stage 'test-agent'", "ended 4 turns without a tool call"],
    );
    assert!(result.output.is_none());

    let nudges = sink.by_kind("nudge");
    assert_eq!(nudges.len(), 3, "one nudge row per bounded silent turn");
    for (index, row) in nudges.iter().enumerate() {
        assert!(
            row.contains(&format!("{}/3", index + 1)),
            "nudge row {index} must name its turn/count, got: {row}"
        );
    }

    let corrective: Vec<String> = sink
        .by_kind("llm_request")
        .into_iter()
        .filter(|content| content.contains("Your turn ended without a tool call"))
        .collect();
    assert_eq!(
        corrective.len(),
        3,
        "exactly 3 nudge followups at the default bound"
    );

    // The loop stopped at the bound: 3 nudged turns + 1 failing turn.
    assert_eq!(mock.requests().len(), 4);
}

#[tokio::test]
async fn nudge_bound_is_configurable_via_max_output_retries() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    let mut mock = RecordingMockLlm::new("test-model");
    for _ in 0..3 {
        mock = mock.with_response(text_response("Prose again."));
    }
    let mock = Arc::new(mock);

    let sink = Arc::new(RecordingSink::new());
    let runner = ADKRunner::new(Arc::clone(&mock) as Arc<dyn Llm>).with_sink(sink.clone());
    let mut task = common::make_task_definition(workspace, true);
    task.adc.max_output_retries = 1;
    let result = runner.execute(task).await.unwrap();

    assert_contract_failure(
        &result,
        &["stage 'test-agent'", "ended 2 turns without a tool call"],
    );
    assert_eq!(
        sink.by_kind("nudge").len(),
        1,
        "bound 1 means exactly 1 nudge"
    );
}

#[tokio::test]
async fn silent_turn_with_retired_marker_never_completes_but_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    // AC-11 behavioral arm: marker text in a no-tool-call turn is just prose —
    // it neither completes the stage nor triggers WaitingForInput. The stage
    // only completes when the verdict tool is actually called.
    let mock = Arc::new(
        RecordingMockLlm::new("test-model")
            .with_response(text_response("All requirements captured. ::TURBOJET:DONE::"))
            .with_response(tool_call_response(
                VERDICT_TOOL_NAME,
                json!({"verdict": "success", "content": "Captured via the tool.", "summary": "done"}),
            )),
    );

    let sink = Arc::new(RecordingSink::new());
    let runner = ADKRunner::new(Arc::clone(&mock) as Arc<dyn Llm>).with_sink(sink.clone());
    let task = common::make_task_definition(workspace, true);
    let result = runner.execute(task).await.unwrap();
    let output = common::assert_success(result);
    assert_eq!(output["verdict"], "success");

    // The marker turn was treated as a silent turn and nudged.
    assert_eq!(sink.by_kind("nudge").len(), 1);
}

#[tokio::test]
async fn non_interactive_silent_final_text_still_parses_as_artifact() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    // FR-10 regression: a no-tool-call turn remains the non-interactive happy
    // path — its text flows through extract_json with no nudge round-trip.
    let mock = Arc::new(
        RecordingMockLlm::new("test-model").with_response(text_response(
            r#"{"status": "ok", "content": "Analysis complete.", "summary": "done"}"#,
        )),
    );

    let sink = Arc::new(RecordingSink::new());
    let runner = ADKRunner::new(Arc::clone(&mock) as Arc<dyn Llm>).with_sink(sink.clone());
    let task = common::make_task_definition(workspace, false);
    let result = runner.execute(task).await.unwrap();
    let output = common::assert_success(result);
    assert_eq!(output["status"], "ok");
    assert_eq!(output["content"], "Analysis complete.");

    // The nudge is gated on interactive: no nudge rows, and the loop returned
    // on the very first turn instead of nudging.
    assert!(sink.by_kind("nudge").is_empty());
    assert_eq!(mock.requests().len(), 1);
}
