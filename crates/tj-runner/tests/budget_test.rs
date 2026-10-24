//! TJ-016: bounded model/tool/correction work at the runner boundary.
//! Every model call, every call in a multi-call tool response, and every
//! output correction is charged before it runs; denial is a visible stage
//! failure. Question asks use their separate stage budget.

mod common;

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use adk_core::{Content, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part};
use async_trait::async_trait;
use serde_json::{Value, json};
use tj_runner::adk_runner::{ADKRunner, QUESTION_TOOL_NAME, VERDICT_TOOL_NAME};
use tj_runner::types::{
    BudgetGuard, QuestionAnswer, QuestionChannel, QuestionRequest, Runner, StageStatus,
};

struct ScriptedLlm {
    name: String,
    responses: Mutex<VecDeque<LlmResponse>>,
}

impl ScriptedLlm {
    fn new(responses: Vec<LlmResponse>) -> Self {
        Self {
            name: "scripted".into(),
            responses: Mutex::new(VecDeque::from(responses)),
        }
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
        let response = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted model ran out of responses");
        Ok(Box::pin(async_stream::stream! { yield Ok(response); }))
    }
}

fn tool_call(name: &str, args: Value) -> LlmResponse {
    LlmResponse::new(Content {
        role: "model".into(),
        parts: vec![Part::FunctionCall {
            name: name.into(),
            args,
            id: Some("call".into()),
            thought_signature: None,
        }],
    })
}

fn multi_call(calls: Vec<(&str, Value)>) -> LlmResponse {
    LlmResponse::new(Content {
        role: "model".into(),
        parts: calls
            .into_iter()
            .map(|(name, args)| Part::FunctionCall {
                name: name.into(),
                args,
                id: Some(format!("call-{name}")),
                thought_signature: None,
            })
            .collect(),
    })
}

fn text(text: &str) -> LlmResponse {
    LlmResponse::new(Content::new("model").with_text(text))
}

#[derive(Default)]
struct RecordingBudget {
    limits: Mutex<HashMap<String, u32>>,
    charges: Mutex<Vec<(String, String)>>,
}

impl RecordingBudget {
    fn with_limits(limits: &[(&str, u32)]) -> Self {
        Self {
            limits: Mutex::new(
                limits
                    .iter()
                    .map(|(kind, limit)| ((*kind).into(), *limit))
                    .collect(),
            ),
            charges: Mutex::new(Vec::new()),
        }
    }

    fn charges(&self, kind: &str) -> usize {
        self.charges
            .lock()
            .unwrap()
            .iter()
            .filter(|(k, _)| k == kind)
            .count()
    }
}

impl BudgetGuard for RecordingBudget {
    fn charge(&self, kind: &str, scope: &str, operation_key: &str) -> anyhow::Result<()> {
        let mut charges = self.charges.lock().unwrap();
        if charges.iter().any(|(_, key)| key == operation_key) {
            return Ok(());
        }
        let limit = self
            .limits
            .lock()
            .unwrap()
            .get(kind)
            .copied()
            .unwrap_or(u32::MAX);
        let used = charges.iter().filter(|(k, _)| k == kind).count() as u32;
        if used >= limit {
            anyhow::bail!("budget exhausted: {} ({}/{})", kind, used, limit);
        }
        charges.push((kind.to_string(), operation_key.to_string()));
        let _ = scope;
        Ok(())
    }
}

struct OneAnswer;
#[async_trait]
impl QuestionChannel for OneAnswer {
    async fn ask(
        &self,
        _stage_id: &str,
        _stage_name: &str,
        _request: &QuestionRequest,
    ) -> anyhow::Result<QuestionAnswer> {
        Ok(QuestionAnswer {
            selected: vec![],
            text: Some("answer".into()),
        })
    }
}

#[tokio::test]
async fn model_budget_stops_retries_with_an_explicit_reason_and_charges_each_call() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Arc::new(ScriptedLlm::new(vec![text("{\"verdict\":\"success\"}")]));
    let budget = Arc::new(RecordingBudget::with_limits(&[(
        tj_db::BUDGET_MODEL_CALLS,
        0,
    )]));
    let runner = ADKRunner::new(mock as Arc<dyn Llm>)
        .with_budget(Arc::clone(&budget) as Arc<dyn BudgetGuard>);
    let result = runner
        .execute(common::make_task_definition(dir.path(), false))
        .await
        .unwrap();
    match result.status {
        StageStatus::Failed { reason } => {
            assert!(reason.contains("budget exhausted: model_calls"), "{reason}")
        }
        other => panic!("expected budget failure, got {other:?}"),
    }
    assert_eq!(budget.charges(tj_db::BUDGET_MODEL_CALLS), 0);
}

#[tokio::test]
async fn every_call_in_a_multi_call_response_is_charged_and_denial_stops_before_later_calls() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "a").unwrap();
    std::fs::write(dir.path().join("b.txt"), "b").unwrap();
    let mock = Arc::new(ScriptedLlm::new(vec![multi_call(vec![
        ("read_file", json!({"path": "a.txt"})),
        ("read_file", json!({"path": "b.txt"})),
    ])]));
    let budget = Arc::new(RecordingBudget::with_limits(&[
        (tj_db::BUDGET_MODEL_CALLS, 10),
        (tj_db::BUDGET_TOOL_CALLS, 1),
    ]));
    let runner = ADKRunner::new(mock as Arc<dyn Llm>)
        .with_budget(Arc::clone(&budget) as Arc<dyn BudgetGuard>);
    let result = runner
        .execute(common::make_task_definition(dir.path(), false))
        .await
        .unwrap();
    match result.status {
        StageStatus::Failed { reason } => {
            assert!(reason.contains("budget exhausted: tool_calls"), "{reason}")
        }
        other => panic!("expected tool budget failure, got {other:?}"),
    }
    assert_eq!(
        budget.charges(tj_db::BUDGET_TOOL_CALLS),
        1,
        "the second call must not execute"
    );
}

#[tokio::test]
async fn malformed_output_corrections_are_separate_from_questions_and_model_calls() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Arc::new(ScriptedLlm::new(vec![
        tool_call(
            QUESTION_TOOL_NAME,
            json!({"question": "Pick one", "options": [{"id": "a", "label": "A"}]}),
        ),
        tool_call(VERDICT_TOOL_NAME, json!({"missing": "verdict"})),
        tool_call(
            VERDICT_TOOL_NAME,
            json!({"verdict": "success", "content": "ok"}),
        ),
    ]));
    let budget = Arc::new(RecordingBudget::with_limits(&[
        (tj_db::BUDGET_MODEL_CALLS, 10),
        (tj_db::BUDGET_OUTPUT_CORRECTIONS, 1),
        (tj_db::BUDGET_QUESTION_ROUNDS, 1),
    ]));
    let runner = ADKRunner::new(mock as Arc<dyn Llm>)
        .with_budget(Arc::clone(&budget) as Arc<dyn BudgetGuard>)
        .with_question_channel(Arc::new(OneAnswer));
    let result = runner
        .execute(common::make_task_definition(dir.path(), true))
        .await
        .unwrap();
    common::assert_success(result);
    assert_eq!(budget.charges(tj_db::BUDGET_MODEL_CALLS), 3);
    assert_eq!(budget.charges(tj_db::BUDGET_OUTPUT_CORRECTIONS), 1);
    assert_eq!(
        budget.charges(tj_db::BUDGET_QUESTION_ROUNDS),
        0,
        "valid question interaction is not an output correction"
    );
}
