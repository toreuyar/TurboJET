//! T-2 acceptance tests: interactive `verdict`/`question` tool declaration
//! gating, base-map persistence across tool-loop iterations, question payload
//! serde round-trips, and the interim not-wired stub interception.

mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use adk_core::{Content, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part};
use async_trait::async_trait;
use serde_json::{Value, json};
use tj_runner::adk_runner::{ADKRunner, QUESTION_TOOL_NAME, VERDICT_TOOL_NAME};
use tj_runner::types::{QuestionAnswer, QuestionChannel, QuestionOption, QuestionRequest, Runner};

/// Sequential response mock that records every `LlmRequest` (conversation +
/// tool declarations) so tests can assert what the model actually received.
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

fn sample_question_request() -> QuestionRequest {
    QuestionRequest {
        question: "Which storage engine should we use?".to_string(),
        options: vec![
            QuestionOption {
                id: "sqlite".to_string(),
                label: "SQLite".to_string(),
            },
            QuestionOption {
                id: "postgres".to_string(),
                label: "PostgreSQL".to_string(),
            },
        ],
        multi_select: false,
        free_form: true,
    }
}

#[tokio::test]
async fn interactive_declarations_present_on_every_iteration() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    std::fs::write(workspace.join("data.txt"), "some content").unwrap();

    // Two tool iterations followed by a final verdict call: the declarations
    // must survive the per-iteration `effective_tools` reset (adk_runner
    // run_tool_loop).
    let mock = Arc::new(
        RecordingMockLlm::new("test-model")
            .with_response(tool_call_response("read_file", json!({"path": "data.txt"})))
            .with_response(tool_call_response("bash", json!({"command": "true"})))
            .with_response(tool_call_response(
                VERDICT_TOOL_NAME,
                json!({"verdict": "success", "content": "done"}),
            )),
    );

    let runner = ADKRunner::new(Arc::clone(&mock) as Arc<dyn Llm>);
    let task = common::make_task_definition(workspace, true);
    let result = runner.execute(task).await.unwrap();
    common::assert_success(result);

    let requests = mock.requests();
    assert_eq!(
        requests.len(),
        3,
        "two tool iterations plus the verdict turn"
    );
    for (index, request) in requests.iter().enumerate() {
        for tool in [VERDICT_TOOL_NAME, QUESTION_TOOL_NAME] {
            assert!(
                request.tools.contains_key(tool),
                "request {} is missing the '{tool}' declaration",
                index + 1
            );
        }
        assert!(
            request.tools.contains_key("read_file"),
            "base registry tools must survive the interactive merge"
        );
    }
}

#[tokio::test]
async fn non_interactive_tasks_get_no_interactive_declarations() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    let mock = Arc::new(
        RecordingMockLlm::new("test-model")
            .with_response(tool_call_response("bash", json!({"command": "true"})))
            .with_response(text_response(r#"{"status":"ok","message":"done."}"#)),
    );

    let runner = ADKRunner::new(Arc::clone(&mock) as Arc<dyn Llm>);
    let task = common::make_task_definition(workspace, false);
    let result = runner.execute(task).await.unwrap();
    common::assert_success(result);

    for request in mock.requests() {
        assert!(!request.tools.contains_key(VERDICT_TOOL_NAME));
        assert!(!request.tools.contains_key(QUESTION_TOOL_NAME));
    }
}

#[tokio::test]
async fn verdict_declaration_reflects_response_schema_with_mandatory_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    let mock = Arc::new(
        RecordingMockLlm::new("test-model").with_response(tool_call_response(
            VERDICT_TOOL_NAME,
            json!({"verdict": "proceed", "content": "report"}),
        )),
    );

    let mut task = common::make_task_definition(workspace, true);
    task.response_schema = Some(json!({
        "type": "object",
        "properties": {
            "verdict": {"type": "string", "enum": ["proceed", "reject"]},
            "content": {"type": "string"}
        },
        "required": ["content"]
    }));

    let runner = ADKRunner::new(Arc::clone(&mock) as Arc<dyn Llm>);
    let result = runner.execute(task).await.unwrap();
    common::assert_success(result);

    let requests = mock.requests();
    assert_eq!(requests.len(), 1);
    let verdict = &requests[0].tools[VERDICT_TOOL_NAME];
    let parameters = &verdict["parameters"];
    // The schema's own verdict vocabulary is preserved, not clobbered.
    assert_eq!(parameters["properties"]["verdict"]["type"], "string");
    assert_eq!(
        parameters["properties"]["verdict"]["enum"],
        json!(["proceed", "reject"])
    );
    let required: Vec<&str> = parameters["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(required.contains(&"verdict"));
    assert!(required.contains(&"content"));
}

#[tokio::test]
async fn verdict_declaration_without_schema_documents_minimal_contract() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    let mock = Arc::new(
        RecordingMockLlm::new("test-model").with_response(tool_call_response(
            VERDICT_TOOL_NAME,
            json!({"verdict": "success", "content": "done"}),
        )),
    );

    let task = common::make_task_definition(workspace, true);
    assert!(task.response_schema.is_none());

    let runner = ADKRunner::new(Arc::clone(&mock) as Arc<dyn Llm>);
    runner.execute(task).await.unwrap();

    let requests = mock.requests();
    assert_eq!(requests.len(), 1);
    let parameters = &requests[0].tools[VERDICT_TOOL_NAME]["parameters"];
    assert_eq!(parameters["properties"]["verdict"]["type"], "string");
    assert!(parameters["properties"].get("content").is_some());
    assert!(parameters["properties"].get("summary").is_some());
    assert_eq!(parameters["required"], json!(["verdict"]));
}

#[tokio::test]
async fn question_tool_call_is_intercepted_with_not_wired_error() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    let request = sample_question_request();
    let mock = Arc::new(
        RecordingMockLlm::new("test-model")
            .with_response(tool_call_response(
                QUESTION_TOOL_NAME,
                serde_json::to_value(&request).unwrap(),
            ))
            .with_response(tool_call_response(
                VERDICT_TOOL_NAME,
                json!({"verdict": "success", "content": "done"}),
            )),
    );

    let runner = ADKRunner::new(Arc::clone(&mock) as Arc<dyn Llm>);
    let task = common::make_task_definition(workspace, true);
    let result = runner.execute(task).await.unwrap();
    // T-3: valid question args pass validation, get the explicit not-wired
    // tool result, and the loop continues; the verdict call then completes
    // the stage (never WaitingForInput).
    common::assert_success(result);

    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    let responses = function_response_texts(&requests[1]);
    assert_eq!(responses.len(), 1, "exactly one function response expected");
    assert!(responses[0].contains("not wired"), "got: {}", responses[0]);
    assert!(
        responses[0].contains("arguments are valid"),
        "valid question args must pass validation, got: {}",
        responses[0]
    );
}

#[tokio::test]
async fn valid_verdict_tool_call_completes_the_stage() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    let mock = Arc::new(
        RecordingMockLlm::new("test-model").with_response(tool_call_response(
            VERDICT_TOOL_NAME,
            json!({"verdict": "success", "content": "Done.", "summary": "All good."}),
        )),
    );

    let runner = ADKRunner::new(Arc::clone(&mock) as Arc<dyn Llm>);
    let task = common::make_task_definition(workspace, true);
    // T-3: the verdict tool resolves as stage completion — the tool arguments
    // ARE the output artifact (the T-2 interim not-wired stub is gone).
    let result = runner.execute(task).await.unwrap();
    let output = common::assert_success(result);
    assert_eq!(output["verdict"], "success");
    assert_eq!(output["content"], "Done.");
    assert_eq!(output["summary"], "All good.");

    // Completion happens inside the loop: exactly one LLM round-trip.
    assert_eq!(mock.requests().len(), 1);
}

#[tokio::test]
async fn malformed_question_arguments_report_parse_failure() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    // Missing the required "question" field and invalid option items.
    let malformed = json!({
        "options": [{"label": "no id here"}],
        "multi_select": false,
        "free_form": false
    });
    let mock = Arc::new(
        RecordingMockLlm::new("test-model")
            .with_response(tool_call_response(QUESTION_TOOL_NAME, malformed))
            .with_response(tool_call_response(
                VERDICT_TOOL_NAME,
                json!({"verdict": "success", "content": "done"}),
            )),
    );

    let runner = ADKRunner::new(Arc::clone(&mock) as Arc<dyn Llm>);
    let task = common::make_task_definition(workspace, true);
    // T-3: malformed question args enter the poke loop (bounded); the verdict
    // call then completes the stage — it must never wait for input.
    let result = runner.execute(task).await.unwrap();
    common::assert_success(result);

    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    let responses = function_response_texts(&requests[1]);
    assert_eq!(responses.len(), 1);
    assert!(
        responses[0].contains("output contract violation"),
        "malformed args must be reported as a contract violation, got: {}",
        responses[0]
    );
    assert!(
        responses[0].contains("failed to deserialize into QuestionRequest"),
        "malformed args must be reported as a parse failure, got: {}",
        responses[0]
    );
}

#[test]
fn question_request_serde_round_trip() {
    let request = sample_question_request();

    let serialized = serde_json::to_string(&request).unwrap();
    let deserialized: QuestionRequest = serde_json::from_str(&serialized).unwrap();
    assert_eq!(deserialized, request);

    // Optional flags default to false when omitted.
    let minimal: QuestionRequest =
        serde_json::from_value(json!({"question": "Continue?", "options": []})).unwrap();
    assert_eq!(minimal.question, "Continue?");
    assert!(minimal.options.is_empty());
    assert!(!minimal.multi_select);
    assert!(!minimal.free_form);

    // F-1: a free-form ask may omit `options` entirely; the parse must not fail.
    let options_absent: QuestionRequest =
        serde_json::from_value(json!({"question": "Any notes?", "free_form": true})).unwrap();
    assert_eq!(options_absent.question, "Any notes?");
    assert!(options_absent.options.is_empty());
    assert!(!options_absent.multi_select);
    assert!(options_absent.free_form);
}

#[test]
fn question_answer_serde_round_trip() {
    let with_text = QuestionAnswer {
        selected: vec!["sqlite".to_string()],
        text: Some("plus a note".to_string()),
    };
    let serialized = serde_json::to_string(&with_text).unwrap();
    let deserialized: QuestionAnswer = serde_json::from_str(&serialized).unwrap();
    assert_eq!(deserialized, with_text);

    let without_text = QuestionAnswer {
        selected: vec!["a".to_string(), "c".to_string()],
        text: None,
    };
    let serialized = serde_json::to_string(&without_text).unwrap();
    let deserialized: QuestionAnswer = serde_json::from_str(&serialized).unwrap();
    assert_eq!(deserialized, without_text);

    // Missing keys fall back to defaults instead of failing the parse.
    let sparse: QuestionAnswer = serde_json::from_value(json!({})).unwrap();
    assert!(sparse.selected.is_empty());
    assert!(sparse.text.is_none());
}

struct CannedChannel;

#[async_trait]
impl QuestionChannel for CannedChannel {
    async fn ask(
        &self,
        _stage_id: &str,
        _stage_name: &str,
        _request: &QuestionRequest,
    ) -> anyhow::Result<QuestionAnswer> {
        Ok(QuestionAnswer {
            selected: vec!["sqlite".to_string()],
            text: Some("user note".to_string()),
        })
    }
}

#[test]
fn with_question_channel_attaches_and_getter_returns_it() {
    let bare = ADKRunner::new(Arc::new(RecordingMockLlm::new("test-model")));
    assert!(bare.question_channel().is_none());

    let attached = ADKRunner::new(Arc::new(RecordingMockLlm::new("test-model")))
        .with_question_channel(Arc::new(CannedChannel));
    assert!(attached.question_channel().is_some());
}

#[tokio::test]
async fn canned_channel_round_trips_request_into_answer() {
    let request = sample_question_request();
    let answer = CannedChannel
        .ask("stage-1", "stage-a", &request)
        .await
        .unwrap();
    assert_eq!(answer.selected, vec!["sqlite".to_string()]);
    assert_eq!(answer.text.as_deref(), Some("user note"));
}
