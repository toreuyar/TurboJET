//! T-3 acceptance tests: verdict resolution → validated artifact, the single
//! completion decision shared by `execute` and `send_followup`, the bounded
//! poke loop (`max_output_retries`), marker retirement (FR-9), and
//! `question`-argument validation at the dispatch site.

mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use adk_core::{Content, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part};
use async_trait::async_trait;
use serde_json::{Value, json};
use tj_runner::adk_runner::{ADKRunner, QUESTION_TOOL_NAME, VERDICT_TOOL_NAME};
use tj_runner::types::{Runner, StageStatus};

use common::RecordingSink;

/// Sequential response mock that records every `LlmRequest` so tests can
/// assert the corrective feedback the model actually received.
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

fn function_response_texts(request: &LlmRequest) -> Vec<String> {
    request
        .contents
        .iter()
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| match part {
            Part::FunctionResponse {
                function_response, ..
            } => Some(function_response.response.to_string()),
            _ => None,
        })
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
async fn valid_verdict_call_completes_with_validated_artifact() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    let mock = Arc::new(
        RecordingMockLlm::new("test-model").with_response(tool_call_response(
            VERDICT_TOOL_NAME,
            json!({"verdict": "success", "content": "Requirements extracted.", "summary": "done"}),
        )),
    );

    let runner = ADKRunner::new(Arc::clone(&mock) as Arc<dyn Llm>);
    let task = common::make_task_definition(workspace, true);
    let result = runner.execute(task).await.unwrap();
    let output = common::assert_success(result);

    // The tool arguments ARE the artifact, verdict key included.
    assert_eq!(output["verdict"], "success");
    assert_eq!(output["content"], "Requirements extracted.");
    assert_eq!(output["summary"], "done");
}

#[tokio::test]
async fn valid_verdict_against_response_schema_completes() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    let mock = Arc::new(
        RecordingMockLlm::new("test-model").with_response(tool_call_response(
            VERDICT_TOOL_NAME,
            json!({"verdict": "proceed", "content": "Evaluation report."}),
        )),
    );

    let mut task = common::make_task_definition(workspace, true);
    task.response_schema = Some(json!({
        "type": "object",
        "properties": {
            "verdict": {"type": "string", "enum": ["proceed", "reject"]},
            "content": {"type": "string"}
        },
        "required": ["verdict", "content"]
    }));

    let sink = Arc::new(RecordingSink::new());
    let runner = ADKRunner::new(Arc::clone(&mock) as Arc<dyn Llm>).with_sink(sink.clone());
    let result = runner.execute(task).await.unwrap();
    let output = common::assert_success(result);
    assert_eq!(output["verdict"], "proceed");

    // The successful validation attempt is recorded.
    let validations = sink.by_kind("output_validation");
    assert_eq!(validations, vec!["ok".to_string()]);
}

#[tokio::test]
async fn schema_invalid_verdict_is_poked_then_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    let mock = Arc::new(
        RecordingMockLlm::new("test-model")
            .with_response(tool_call_response(
                VERDICT_TOOL_NAME,
                json!({"verdict": "maybe", "content": "unsure"}),
            ))
            .with_response(tool_call_response(
                VERDICT_TOOL_NAME,
                json!({"verdict": "reject", "content": "final answer"}),
            )),
    );

    let mut task = common::make_task_definition(workspace, true);
    task.response_schema = Some(json!({
        "type": "object",
        "properties": {
            "verdict": {"type": "string", "enum": ["proceed", "reject"]},
            "content": {"type": "string"}
        },
        "required": ["verdict", "content"]
    }));

    let sink = Arc::new(RecordingSink::new());
    let runner = ADKRunner::new(Arc::clone(&mock) as Arc<dyn Llm>).with_sink(sink.clone());
    let result = runner.execute(task).await.unwrap();
    let output = common::assert_success(result);
    assert_eq!(output["verdict"], "reject");

    // The corrective round fed the schema violation back into the SAME
    // session conversation, then recorded the recovered validation.
    let corrective: Vec<String> = sink
        .by_kind("llm_request")
        .into_iter()
        .filter(|content| content.contains("output contract violation"))
        .collect();
    assert_eq!(
        corrective.len(),
        1,
        "exactly one corrective round: {:?}",
        corrective
    );
    assert!(
        corrective[0].contains("schema violation"),
        "got: {}",
        corrective[0]
    );
    assert!(
        corrective[0].contains("respond again, calling the verdict tool"),
        "poke must instruct the retry, got: {}",
        corrective[0]
    );

    let validations = sink.by_kind("output_validation");
    assert_eq!(validations.len(), 2);
    assert!(validations[0].contains("schema violation"));
    assert_eq!(validations[1], "ok");

    // The model saw the violation as its tool result before retrying.
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    let responses = function_response_texts(&requests[1]);
    assert!(responses[0].contains("output contract violation: schema violation"));
}

#[tokio::test]
async fn invalid_verdict_exhausts_default_bound_and_fails_visibly() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    // Default bound 3: three corrective rounds, then the 4th failed attempt
    // fails the stage visibly.
    let mut mock = RecordingMockLlm::new("test-model");
    for _ in 0..4 {
        mock = mock.with_response(tool_call_response(
            VERDICT_TOOL_NAME,
            json!({"content": "no verdict key here"}),
        ));
    }
    let mock = Arc::new(mock);

    let sink = Arc::new(RecordingSink::new());
    let runner = ADKRunner::new(Arc::clone(&mock) as Arc<dyn Llm>).with_sink(sink.clone());
    let task = common::make_task_definition(workspace, true);
    let result = runner.execute(task).await.unwrap();

    assert_contract_failure(
        &result,
        &["stage 'test-agent'", "after 4 attempts", "missing verdict"],
    );
    assert!(result.output.is_none());

    let corrective: Vec<String> = sink
        .by_kind("llm_request")
        .into_iter()
        .filter(|content| content.contains("output contract violation"))
        .collect();
    assert_eq!(
        corrective.len(),
        3,
        "exactly 3 corrective rounds at the default bound"
    );
    assert_eq!(sink.by_kind("output_validation").len(), 4);
}

#[tokio::test]
async fn poke_bound_is_configurable_via_max_output_retries() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    let mut mock = RecordingMockLlm::new("test-model");
    for _ in 0..2 {
        mock = mock.with_response(tool_call_response(
            VERDICT_TOOL_NAME,
            json!({"content": "still no verdict"}),
        ));
    }
    let mock = Arc::new(mock);

    let sink = Arc::new(RecordingSink::new());
    let runner = ADKRunner::new(Arc::clone(&mock) as Arc<dyn Llm>).with_sink(sink.clone());
    let mut task = common::make_task_definition(workspace, true);
    task.adc.max_output_retries = 1;
    let result = runner.execute(task).await.unwrap();

    assert_contract_failure(&result, &["stage 'test-agent'", "after 2 attempts"]);
    let corrective: Vec<String> = sink
        .by_kind("llm_request")
        .into_iter()
        .filter(|content| content.contains("output contract violation"))
        .collect();
    assert_eq!(
        corrective.len(),
        1,
        "bound 1 means exactly 1 corrective round"
    );
}

// The T-3 interim prose-turn failure tests were replaced by T-4's
// silent-turn nudge tests (`interactive_nudge_test.rs`): a no-tool-call turn
// is now nudged back into the tool contract and only fails after the nudge
// bound — never Success, never WaitingForInput.

#[tokio::test]
async fn tool_loop_exhaustion_without_verdict_fails_visibly() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    std::fs::write(workspace.join("data.txt"), "content").unwrap();

    let mut mock = RecordingMockLlm::new("test-model");
    for _ in 0..20 {
        mock = mock.with_response(tool_call_response("read_file", json!({"path": "data.txt"})));
    }
    let mock = Arc::new(mock);

    let runner = ADKRunner::new(Arc::clone(&mock) as Arc<dyn Llm>);
    let task = common::make_task_definition(workspace, true);
    let result = runner.execute(task).await.unwrap();

    // MAX_TOOL_ITERATIONS exhaustion is a distinct, non-completing outcome:
    // visible failure, never Success/WaitingForInput.
    assert_contract_failure(
        &result,
        &[
            "stage 'test-agent'",
            "exhausted the tool-loop cap",
            "20 iterations",
        ],
    );
}

#[tokio::test]
async fn verdict_after_user_input_completes_via_followup() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    // FR-14: the followup path funnels through the same resolution helper —
    // including schema validation via the session's stored response schema.
    // T-4 note: the stage's first execution fails via the silent-turn nudge
    // bound (a prose-only turn is nudged, not failed outright), keeping the
    // session addressable for the followup.
    let mock = Arc::new(
        RecordingMockLlm::new("test-model")
            // First execution: nudge bound 1 → two silent turns exhaust it.
            .with_response(text_response("Here is what I understood so far."))
            .with_response(text_response("Still thinking in prose."))
            // Followup turn 1: verdict value outside the schema's enum → poke.
            .with_response(tool_call_response(
                VERDICT_TOOL_NAME,
                json!({"verdict": "proceed", "content": "wrong route"}),
            ))
            // Followup turn 2: valid → completion.
            .with_response(tool_call_response(
                VERDICT_TOOL_NAME,
                json!({"verdict": "success", "content": "Extraction complete."}),
            )),
    );

    let sink = Arc::new(RecordingSink::new());
    let runner = ADKRunner::new(Arc::clone(&mock) as Arc<dyn Llm>).with_sink(sink.clone());
    let mut task = common::make_task_definition(workspace, true);
    task.adc.max_output_retries = 1;
    task.response_schema = Some(json!({
        "type": "object",
        "properties": {
            "verdict": {"type": "string", "enum": ["success"]},
            "content": {"type": "string"}
        },
        "required": ["verdict", "content"]
    }));

    let first = runner.execute(task).await.unwrap();
    let session_id = match &first.status {
        StageStatus::Failed { reason } => {
            assert!(reason.contains("output contract violation"));
            first
                .connection
                .as_ref()
                .and_then(|c| c.session_id.clone())
                .expect("interactive failure keeps the session addressable")
        }
        other => panic!(
            "expected the nudge-bound failure to fail the stage, got {:?}",
            other
        ),
    };

    let second = runner
        .send_followup(&session_id, "The missing detail is X.")
        .await
        .unwrap();
    let output = common::assert_success(second);
    assert_eq!(output["verdict"], "success");
    assert_eq!(output["content"], "Extraction complete.");

    // The followup-side poke validated against the SAME schema.
    let corrective: Vec<String> = sink
        .by_kind("llm_request")
        .into_iter()
        .filter(|content| content.contains("schema violation"))
        .collect();
    assert_eq!(
        corrective.len(),
        1,
        "followup poke recorded: {:?}",
        corrective
    );
}

#[tokio::test]
async fn followup_invalid_verdict_exhausts_bound_and_fails() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    // T-4 note: the first execution fails via the verdict poke bound (a
    // prose-only turn would now be nudged, not failed), keeping the session
    // addressable for the followup, which exhausts its own fresh bound.
    let mock = Arc::new(
        RecordingMockLlm::new("test-model")
            .with_response(tool_call_response(
                VERDICT_TOOL_NAME,
                json!({"content": "no verdict"}),
            ))
            .with_response(tool_call_response(
                VERDICT_TOOL_NAME,
                json!({"content": "still none"}),
            ))
            .with_response(tool_call_response(
                VERDICT_TOOL_NAME,
                json!({"content": "followup none"}),
            ))
            .with_response(tool_call_response(
                VERDICT_TOOL_NAME,
                json!({"content": "followup still none"}),
            )),
    );

    let runner = ADKRunner::new(Arc::clone(&mock) as Arc<dyn Llm>);
    let mut task = common::make_task_definition(workspace, true);
    task.adc.max_output_retries = 1;

    let first = runner.execute(task).await.unwrap();
    let session_id = match &first.status {
        StageStatus::Failed { .. } => first
            .connection
            .as_ref()
            .and_then(|c| c.session_id.clone())
            .expect("session addressable after failed first execution"),
        other => panic!("expected the first execution to fail, got {:?}", other),
    };

    let second = runner
        .send_followup(&session_id, "here is the answer")
        .await
        .unwrap();
    assert_contract_failure(
        &second,
        &["stage 'test-agent'", "after 2 attempts", "missing verdict"],
    );
}

#[tokio::test]
async fn degenerate_question_args_poked_then_fail_visibly() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    // No options and free_form=false: the user has no way to answer. The ask
    // must be rejected by validation — never blocking in receive().
    let degenerate = json!({
        "question": "Pick one",
        "options": [],
        "multi_select": false,
        "free_form": false
    });
    let mut mock = RecordingMockLlm::new("test-model");
    for _ in 0..4 {
        mock = mock.with_response(tool_call_response(QUESTION_TOOL_NAME, degenerate.clone()));
    }
    let mock = Arc::new(mock);

    let sink = Arc::new(RecordingSink::new());
    let runner = ADKRunner::new(Arc::clone(&mock) as Arc<dyn Llm>).with_sink(sink.clone());
    let task = common::make_task_definition(workspace, true);
    let result = runner.execute(task).await.unwrap();

    assert_contract_failure(
        &result,
        &[
            "stage 'test-agent'",
            "after 4 attempts",
            "degenerate question",
        ],
    );

    let corrective: Vec<String> = sink
        .by_kind("llm_request")
        .into_iter()
        .filter(|content| content.contains("output contract violation"))
        .collect();
    assert_eq!(corrective.len(), 3);

    // The pokes reached the model as tool results (same-session retry).
    let requests = mock.requests();
    assert_eq!(requests.len(), 4);
    let responses = function_response_texts(&requests[1]);
    assert!(responses[0].contains("degenerate question"));
    assert!(responses[0].contains("output contract violation"));
}
