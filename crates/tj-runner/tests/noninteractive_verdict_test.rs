//! T-9 verification: the runner-appended non-interactive Output Contract names
//! the `verdict` key, and a non-interactive stage whose final JSON carries a
//! route-valid `verdict` completes with the verdict in its artifact (the
//! extract_json + execute_with_retry channel is untouched, FR-10).

mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use adk_core::{Content, Llm, LlmRequest, LlmResponse, LlmResponseStream};
use async_trait::async_trait;
use serde_json::json;
use tj_runner::adk_runner::ADKRunner;
use tj_runner::types::{Runner, StageStatus};

/// Sequential response mock that records every `LlmRequest` so tests can
/// assert what the model actually received (same pattern as
/// interactive_declarations_test.rs).
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

fn text_response(text: &str) -> LlmResponse {
    LlmResponse::new(Content::new("model").with_text(text))
}

fn request_text(req: &LlmRequest) -> String {
    req.contents
        .iter()
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| part.text())
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn output_contract_appended_to_non_interactive_tasks_mentions_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let llm = Arc::new(
        RecordingMockLlm::new("test-model").with_response(text_response(
            r#"{"verdict": "success", "summary": "done", "content": "report"}"#,
        )),
    );
    let runner = ADKRunner::new(llm.clone());
    let task = common::make_task_definition(dir.path(), false);
    let result = runner.execute(task).await.unwrap();
    assert!(
        matches!(result.status, StageStatus::Success),
        "expected Success, got {:?}",
        result.status
    );

    let requests = llm.requests();
    assert!(!requests.is_empty(), "at least one LLM request was made");
    let first_text = request_text(&requests[0]);
    assert!(
        first_text.contains("## Output Contract"),
        "non-interactive prompt must carry the appended Output Contract, got:\n{first_text}"
    );
    assert!(
        first_text.contains("\"verdict\""),
        "Output Contract must name the verdict key, got:\n{first_text}"
    );
}

#[tokio::test]
async fn non_interactive_schema_violation_is_corrected_with_precise_error() {
    let dir = tempfile::tempdir().unwrap();
    let llm = Arc::new(
        RecordingMockLlm::new("test-model")
            // Parseable JSON, but the required summary is missing.
            .with_response(text_response(r#"{"verdict":"success"}"#))
            .with_response(text_response(
                r#"{"verdict":"success","summary":"fixed","items":["ok"]}"#,
            )),
    );
    let runner = ADKRunner::new(llm.clone());
    let mut task = common::make_task_definition(dir.path(), false);
    task.response_schema = Some(json!({
        "type": "object",
        "required": ["verdict", "summary", "items"],
        "properties": {
            "verdict": {"type": "string", "enum": ["success"]},
            "summary": {"type": "string"},
            "items": {"type": "array", "items": {"type": "string"}}
        }
    }));
    let output = common::assert_success(runner.execute(task).await.unwrap());
    assert_eq!(output["summary"], "fixed");
    let requests = llm.requests();
    assert_eq!(requests.len(), 2, "one bounded correction round");
    let correction = request_text(&requests[1]);
    assert!(
        correction.contains("required field 'summary' missing"),
        "{correction}"
    );
}

#[tokio::test]
async fn non_interactive_invalid_array_item_cannot_complete() {
    let dir = tempfile::tempdir().unwrap();
    let llm = Arc::new(
        RecordingMockLlm::new("test-model")
            .with_response(text_response(
                r#"{"verdict":"success","summary":"x","items":["ok",7]}"#,
            ))
            .with_response(text_response(
                r#"{"verdict":"success","summary":"x","items":["ok","fixed"]}"#,
            )),
    );
    let runner = ADKRunner::new(llm.clone());
    let mut task = common::make_task_definition(dir.path(), false);
    task.response_schema = Some(json!({
        "type": "object",
        "required": ["verdict", "summary", "items"],
        "properties": {
            "verdict": {"type": "string", "enum": ["success"]},
            "summary": {"type": "string"},
            "items": {"type": "array", "items": {"type": "string"}}
        }
    }));
    let output = common::assert_success(runner.execute(task).await.unwrap());
    assert_eq!(output["items"][1], "fixed");
    let correction = request_text(&llm.requests()[1]);
    assert!(correction.contains("items.1"), "{correction}");
    assert!(
        correction.contains("expected type 'string'"),
        "{correction}"
    );
}

#[tokio::test]
async fn non_interactive_schema_failure_after_bound_is_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let llm = Arc::new(
        RecordingMockLlm::new("test-model")
            .with_response(text_response(r#"{"verdict":"success"}"#)),
    );
    let runner = ADKRunner::new(llm);
    let mut task = common::make_task_definition(dir.path(), false);
    task.adc.max_output_retries = 0;
    task.response_schema = Some(json!({
        "type": "object",
        "required": ["verdict", "summary"],
        "properties": {
            "verdict": {"type": "string", "enum": ["success"]},
            "summary": {"type": "string"}
        }
    }));
    let error = runner.execute(task).await.unwrap_err().to_string();
    assert!(error.contains("output contract violation"), "{error}");
    assert!(
        error.contains("required field 'summary' missing"),
        "{error}"
    );
}

#[tokio::test]
async fn non_interactive_final_json_with_route_valid_verdict_completes_with_verdict_artifact() {
    let dir = tempfile::tempdir().unwrap();
    let llm = Arc::new(
        RecordingMockLlm::new("test-model").with_response(text_response(
            r##"{"verdict": "success", "summary": "all checks pass", "content": "# report"}"##,
        )),
    );
    let runner = ADKRunner::new(llm);
    let mut task = common::make_task_definition(dir.path(), false);
    // Schema shaped like synthetic stage schema: verdict enum = the stage's
    // SMG route keys exactly.
    task.response_schema = Some(json!({
        "type": "object",
        "required": ["verdict", "summary"],
        "properties": {
            "verdict": {
                "type": "string",
                "enum": ["success", "failed", "cannot-complete"]
            },
            "summary": { "type": "string" }
        }
    }));
    let result = runner.execute(task).await.unwrap();
    let output = common::assert_success(result);
    assert_eq!(
        output["verdict"], "success",
        "the artifact must carry the route-valid verdict for flow-layer routing"
    );
    assert_eq!(output["summary"], "all checks pass");
}
