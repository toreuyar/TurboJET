use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use tj_config::loader::validate_json_value;
use tj_db::RunnerConnection;
use tj_tools::{ToolContext, ToolRegistry, ToolsetConfig};

use crate::types::{
    BudgetGuard, LlmUsage, QuestionChannel, QuestionRequest, Runner, StageLogSink, StageResult,
    StageStatus, TaskDefinition, UsageSink,
};

/// Deprecated completion protocol (FR-9): interactive stages complete ONLY via
/// the `verdict` tool. The constant is tolerated during the config migration
/// window (T-8 flips the prompts) but is checked by NO code path.
#[allow(dead_code)]
const COMPLETION_MARKER: &str = "::TURBOJET:DONE::";
const MAX_TOOL_ITERATIONS: u32 = 20;

/// Terminal state of [`ADKRunner::run_tool_loop`]. Completion is a decision
/// made inside the loop's dispatch site; every other variant is explicitly
/// non-completing and is consumed by the entry paths (`execute` and
/// `send_followup`).
enum ToolLoopOutcome {
    /// Interactive stage finished: the validated `verdict` tool arguments are
    /// the stage's output artifact (FR-4, FR-14). Mapped to `Success` for both
    /// entry paths by the shared [`ADKRunner::interactive_result`] mapping.
    Completed(Value),
    /// A turn ended with no function calls: the loop's final model text.
    /// Non-completing — the non-interactive JSON channel consumes it exactly
    /// as before (FR-10); an interactive stage never completes on a
    /// prose-only turn (FR-9, marker retired).
    FinalText(String),
    /// MAX_TOOL_ITERATIONS exhausted without a completion decision. Also
    /// non-completing, with the same consumption contract as
    /// [`ToolLoopOutcome::FinalText`].
    MaxToolIterations(String),
    /// The interactive output-contract bound is exhausted: visible failure
    /// whose reason carries the machine-detectable `output contract violation:`
    /// prefix (FR-13, FR-15). Never `WaitingForInput`, never `Success`.
    ContractViolation(String),
}

impl ToolLoopOutcome {
    /// The model-side text recorded into the session history for this outcome.
    fn transcript_text(&self) -> String {
        match self {
            ToolLoopOutcome::Completed(artifact) => artifact.to_string(),
            ToolLoopOutcome::FinalText(text) | ToolLoopOutcome::MaxToolIterations(text) => {
                text.clone()
            }
            ToolLoopOutcome::ContractViolation(reason) => reason.clone(),
        }
    }
}

/// Result of resolving one interactive tool call at the dispatch site — the
/// single completion/validation decision point (FR-4, FR-12, FR-14).
enum InteractiveToolOutcome {
    /// Tool result to feed back into the same session conversation; the loop
    /// continues so the same LLM can retry in-session.
    Feedback(Value),
    /// Validated artifact — the stage completes.
    Completed(Value),
    /// Contract bound exhausted — visible failure with this reason.
    Violation(String),
}

/// Interactive-only completion tool (T-3 wires its resolution).
pub const VERDICT_TOOL_NAME: &str = "verdict";
/// Interactive-only blocking question tool (resolution over the question channel).
pub const QUESTION_TOOL_NAME: &str = "question";

struct RunnerState {
    state: HashMap<String, Value>,
}

impl adk_core::State for RunnerState {
    fn get(&self, key: &str) -> Option<Value> {
        self.state.get(key).cloned()
    }

    fn set(&mut self, key: String, value: Value) {
        if adk_core::validate_state_key(&key).is_ok() {
            self.state.insert(key, value);
        }
    }

    fn all(&self) -> HashMap<String, Value> {
        self.state.clone()
    }
}

struct RunnerSession {
    id: String,
    stage_id: String,
    stage_name: String,
    conversation: Mutex<Vec<adk_core::Content>>,
    state: RunnerState,
    toolset_config: Option<ToolsetConfig>,
    workspace: std::path::PathBuf,
    output_dir: std::path::PathBuf,
    interactive: bool,
    response_schema: Option<serde_json::Value>,
    max_output_retries: u32,
    timeout_secs: u64,
}

impl RunnerSession {
    #[allow(clippy::too_many_arguments)]
    fn new(
        id: String,
        stage_id: String,
        stage_name: String,
        toolset_config: Option<ToolsetConfig>,
        workspace: std::path::PathBuf,
        output_dir: std::path::PathBuf,
        interactive: bool,
        response_schema: Option<serde_json::Value>,
        max_output_retries: u32,
        timeout_secs: u64,
    ) -> Self {
        tracing::debug!(session_id = %id, "Creating RunnerSession");
        Self {
            id,
            stage_id,
            stage_name,
            conversation: Mutex::new(Vec::new()),
            state: RunnerState {
                state: HashMap::new(),
            },
            toolset_config,
            workspace,
            output_dir,
            interactive,
            response_schema,
            max_output_retries,
            timeout_secs,
        }
    }

    fn conversation(&self) -> Vec<adk_core::Content> {
        self.conversation.lock().unwrap().clone()
    }

    fn append(&self, content: adk_core::Content) {
        tracing::debug!(
            session_id = %self.id,
            role = %content.role,
            parts = content.parts.len(),
            "Appending content to session history"
        );
        self.conversation.lock().unwrap().push(content);
    }
}

impl adk_core::Session for RunnerSession {
    fn id(&self) -> &str {
        &self.id
    }

    fn app_name(&self) -> &str {
        "turbojet"
    }

    fn user_id(&self) -> &str {
        "runner"
    }

    fn state(&self) -> &dyn adk_core::State {
        &self.state
    }

    fn conversation_history(&self) -> Vec<adk_core::Content> {
        self.conversation()
    }
}

fn extract_function_calls(
    content: &adk_core::Content,
) -> Vec<(String, serde_json::Value, Option<String>)> {
    content
        .parts
        .iter()
        .filter_map(|part| match part {
            adk_core::types::Part::FunctionCall { name, args, id, .. } => {
                Some((name.clone(), args.clone(), id.clone()))
            }
            _ => None,
        })
        .collect()
}

pub struct ADKRunner {
    model: Arc<dyn adk_core::Llm>,
    model_name: String,
    sessions: Mutex<HashMap<String, Arc<RunnerSession>>>,
    sink: Option<std::sync::Arc<dyn StageLogSink>>,
    question_channel: Option<Arc<dyn QuestionChannel>>,
    budget: Option<Arc<dyn BudgetGuard>>,
    usage: Option<Arc<dyn UsageSink>>,
}

impl ADKRunner {
    pub fn new(model: Arc<dyn adk_core::Llm>) -> Self {
        let model_name = model.name().to_string();
        tracing::debug!(
            model = %model_name,
            "Creating ADKRunner"
        );
        Self {
            model,
            model_name,
            sessions: Mutex::new(HashMap::new()),
            sink: None,
            question_channel: None,
            budget: None,
            usage: None,
        }
    }

    /// Attach the durable run-wide budget. Every model call, every tool call
    /// in a multi-call response, and every output correction is charged before
    /// the effect starts; denial fails the stage visibly instead of continuing.
    pub fn with_budget(mut self, budget: Arc<dyn BudgetGuard>) -> Self {
        self.budget = Some(budget);
        self
    }

    fn charge_budget(&self, kind: &str, scope: &str) -> Result<()> {
        self.charge_budget_with_key(kind, scope, &format!("op_{}", uuid::Uuid::new_v4()))
    }

    fn charge_budget_with_key(&self, kind: &str, scope: &str, operation_key: &str) -> Result<()> {
        if let Some(budget) = &self.budget {
            budget.charge(kind, scope, operation_key)?;
        }
        Ok(())
    }

    /// Attach durable token/cost accounting (TJ-024).
    pub fn with_usage_sink(mut self, sink: Arc<dyn UsageSink>) -> Self {
        self.usage = Some(sink);
        self
    }

    /// Attach an activity sink (LLM traffic + tool calls are reported here).
    pub fn with_sink(mut self, sink: std::sync::Arc<dyn StageLogSink>) -> Self {
        self.sink = Some(sink);
        self
    }

    /// Attach the blocking question channel used by interactive stages. Without
    /// a channel the runner still declares/handles the `question` tool shape;
    /// the stage-side implementation (tj-stage) supplies the real channel.
    pub fn with_question_channel(mut self, channel: Arc<dyn QuestionChannel>) -> Self {
        self.question_channel = Some(channel);
        self
    }

    /// The attached question channel, if any (wiring inspection + T-3 dispatch).
    pub fn question_channel(&self) -> Option<Arc<dyn QuestionChannel>> {
        self.question_channel.clone()
    }

    /// Inspection aid (post-mortem tooling and tests): how many sessions are
    /// currently registered. Sessions are inserted before the tool loop
    /// starts, so a per-call LLM timeout leaves the conversation in the map
    /// instead of erasing it.
    #[doc(hidden)]
    pub fn session_count(&self) -> usize {
        self.sessions.lock().unwrap().len()
    }

    fn report(&self, stage_id: &str, stage_name: &str, kind: &str, content: &str) {
        if let Some(sink) = &self.sink {
            sink.log(stage_id, stage_name, kind, content);
        }
    }

    /// Declarations for the interactive-only `verdict`/`question` tools, in the
    /// same shape [`tj_tools::ToolRegistry::declarations`] produces. These MUST
    /// be merged into the base tool map (never only into a per-iteration copy):
    /// `run_tool_loop` resets `effective_tools` from the base map every
    /// iteration, so a merge anywhere else drops the declarations from
    /// iteration 2 onward.
    fn interactive_declarations(
        response_schema: Option<&serde_json::Value>,
    ) -> HashMap<String, serde_json::Value> {
        HashMap::from([
            (
                VERDICT_TOOL_NAME.to_string(),
                Self::verdict_declaration(response_schema),
            ),
            (QUESTION_TOOL_NAME.to_string(), Self::question_declaration()),
        ])
    }

    /// `verdict` argument schema: the stage's `response_schema` when present
    /// (its own `verdict` property, e.g. an enum of route keys, is preserved),
    /// otherwise the minimal `verdict`/`content`/`summary` contract — plus a
    /// mandatory `verdict: string` property in both cases (FR-3, FR-22).
    fn verdict_declaration(response_schema: Option<&serde_json::Value>) -> serde_json::Value {
        let mut parameters = match response_schema {
            Some(schema) if schema.is_object() => schema.clone(),
            _ => serde_json::json!({
                "type": "object",
                "properties": {
                    "content": {"type": "string"},
                    "summary": {"type": "string"},
                },
            }),
        };
        if let Some(obj) = parameters.as_object_mut() {
            let props = obj
                .entry("properties")
                .or_insert_with(|| serde_json::json!({}));
            if !props.is_object() {
                *props = serde_json::json!({});
            }
            if !props.get("verdict").is_some_and(|v| v.is_object()) {
                if let Some(props_obj) = props.as_object_mut() {
                    props_obj.insert("verdict".to_string(), serde_json::json!({"type": "string"}));
                }
            }
            match obj.get_mut("required").and_then(|r| r.as_array_mut()) {
                Some(required) => {
                    if !required.iter().any(|v| v == "verdict") {
                        required.push(serde_json::Value::String("verdict".to_string()));
                    }
                }
                None => {
                    obj.insert("required".to_string(), serde_json::json!(["verdict"]));
                }
            }
        }
        serde_json::json!({
            "name": VERDICT_TOOL_NAME,
            "description": "Finish this stage. Call exactly once, when your work is complete: the tool \
                arguments ARE the stage's final structured output artifact and MUST include a \
                mandatory \"verdict\" string (with \"content\"/\"summary\" and any stage-specific \
                fields). A turn that ends without a verdict tool call never completes the stage.",
            "parameters": parameters,
        })
    }

    fn question_declaration() -> serde_json::Value {
        serde_json::json!({
            "name": QUESTION_TOOL_NAME,
            "description": "Ask the user a question and block until they answer. Offer multiple-choice \
                options and/or free-form text (at least one of the two must be offered). The call \
                blocks until the user replies; their structured answer arrives as the tool result and \
                the conversation continues in-session. Ask whenever you need input, confirmation or \
                clarification from the user.",
            "parameters": {
                "type": "object",
                "properties": {
                    "question": {
                        "type": "string",
                        "description": "The question text shown to the user."
                    },
                    "options": {
                        "type": "array",
                        "description": "Multiple-choice options; may be empty when only free-form text is wanted.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {
                                    "type": "string",
                                    "description": "Stable option id, echoed back in the answer's selected list."
                                },
                                "label": {
                                    "type": "string",
                                    "description": "Human-readable option label."
                                }
                            },
                            "required": ["id", "label"]
                        }
                    },
                    "multi_select": {
                        "type": "boolean",
                        "description": "Whether the user may select several options."
                    },
                    "free_form": {
                        "type": "boolean",
                        "description": "Whether the user may add free-form text."
                    }
                },
                "required": ["question"]
            }
        })
    }

    /// THE single interactive completion decision (FR-4, FR-12, FR-14): every
    /// `verdict`/`question` call from every entry path resolves here — both
    /// `execute` and `send_followup` funnel through [`ADKRunner::run_tool_loop`],
    /// whose dispatch site is this function's only caller. No marker-era
    /// completion logic remains anywhere.
    ///
    /// Verdict vocabulary note: the runner deliberately does not know the SMG
    /// route vocabulary. A well-formed but unroutable verdict value is rejected
    /// loudly at the flow layer (`resolve_next`) and statically by the config
    /// lint — not here (plan T-3 scope 2).
    #[allow(clippy::too_many_arguments)]
    async fn resolve_interactive_tool(
        &self,
        stage_id: &str,
        stage_name: &str,
        name: &str,
        args: &serde_json::Value,
        response_schema: Option<&serde_json::Value>,
        contract_pokes_used: &mut u32,
        max_output_retries: u32,
    ) -> InteractiveToolOutcome {
        match name {
            VERDICT_TOOL_NAME => match Self::check_verdict_args(args, response_schema) {
                Ok(()) => {
                    self.report(stage_id, stage_name, "output_validation", "ok");
                    InteractiveToolOutcome::Completed(args.clone())
                }
                Err(detail) => self.poke_or_fail(
                    stage_id,
                    stage_name,
                    detail,
                    contract_pokes_used,
                    max_output_retries,
                    "respond again, calling the verdict tool with valid arguments",
                ),
            },
            QUESTION_TOOL_NAME => match Self::check_question_args(args) {
                Ok(request) => {
                    self.report(stage_id, stage_name, "output_validation", "ok");
                    match self.question_channel.as_ref() {
                        // Wired (T-5): the blocking ask runs over the stage
                        // question channel (webui ∥ Zulip, durable status
                        // flips); the parsed QuestionAnswer travels back as
                        // the tool result and the loop continues in-session
                        // (FR-5, FR-6).
                        Some(channel) => match channel.ask(stage_id, stage_name, &request).await {
                            Ok(answer) => {
                                tracing::info!(
                                    stage_id = %stage_id,
                                    stage = %stage_name,
                                    selected = ?answer.selected,
                                    text = ?answer.text,
                                    "Question tool resolved with the user's answer"
                                );
                                InteractiveToolOutcome::Feedback(serde_json::json!({
                                    "success": true,
                                    "answer": answer,
                                }))
                            }
                            Err(error) if error.to_string().contains("budget exhausted:") => {
                                // A question budget denial cannot be cured by
                                // asking again; fail the stage visibly instead
                                // of feeding the denial back as a tool result.
                                InteractiveToolOutcome::Violation(error.to_string())
                            }
                            Err(error) => {
                                tracing::warn!(
                                    stage_id = %stage_id,
                                    stage = %stage_name,
                                    error = %error,
                                    "Question tool ask failed over the stage question channel"
                                );
                                InteractiveToolOutcome::Feedback(serde_json::json!({
                                    "success": false,
                                    "error": error.to_string(),
                                }))
                            }
                        },
                        // No channel attached (chat-less deployment/test):
                        // keep the explicit not-wired result so a valid
                        // question call neither completes the stage nor
                        // blocks in a receive().
                        None => InteractiveToolOutcome::Feedback(serde_json::json!({
                            "success": false,
                            "error": "Tool 'question' is not wired yet: arguments are valid, but the blocking ask over the stage question channel lands in a later task (T-5)",
                        })),
                    }
                }
                Err(detail) => self.poke_or_fail(
                    stage_id,
                    stage_name,
                    detail,
                    contract_pokes_used,
                    max_output_retries,
                    "respond again with a corrected question tool call (non-empty question text; options and/or free_form)",
                ),
            },
            _ => InteractiveToolOutcome::Feedback(serde_json::json!({
                "success": false,
                "error": format!("Unknown interactive tool '{}'", name),
            })),
        }
    }

    /// Shared tail of the output contract (FR-12, FR-13, D-2): on validation
    /// failure, poke the same LLM in the same session conversation while the
    /// per-ADC bound allows it; when the bound is exhausted, surface the
    /// visible `output contract violation:` failure. The user is never asked
    /// anything during this loop (AC-2).
    fn poke_or_fail(
        &self,
        stage_id: &str,
        stage_name: &str,
        detail: String,
        contract_pokes_used: &mut u32,
        max_output_retries: u32,
        remedy: &str,
    ) -> InteractiveToolOutcome {
        self.report(stage_id, stage_name, "output_validation", &detail);
        if *contract_pokes_used >= max_output_retries {
            return InteractiveToolOutcome::Violation(format!(
                "output contract violation: stage '{}' failed validation after {} attempts; last error: {}",
                stage_name,
                *contract_pokes_used + 1,
                detail
            ));
        }
        if let Err(error) = self.charge_budget(tj_db::BUDGET_OUTPUT_CORRECTIONS, stage_id) {
            return InteractiveToolOutcome::Violation(format!(
                "output contract violation: stage '{}' cannot be corrected: {}",
                stage_name, error
            ));
        }
        *contract_pokes_used += 1;
        let poke = format!(
            "output contract violation: {}. Your output contract was violated: {}.",
            detail, remedy
        );
        // One corrective round = one llm_request row (AC-2 evidence).
        self.report(stage_id, stage_name, "llm_request", &poke);
        InteractiveToolOutcome::Feedback(serde_json::json!({
            "success": false,
            "error": poke,
        }))
    }

    /// Validate `verdict` tool arguments as the stage's output artifact
    /// (FR-11, NFR-5): (a) structurally a JSON object, (b) valid against the
    /// ADC's response schema when one is loaded, (c) carrying a non-empty
    /// string `verdict` key. Error strings name the violated constraint so
    /// they can be fed back verbatim as corrective pokes.
    fn check_verdict_args(
        args: &serde_json::Value,
        response_schema: Option<&serde_json::Value>,
    ) -> Result<(), String> {
        if !args.is_object() {
            return Err(format!(
                "parse error: verdict arguments must be a JSON object, found {}",
                Self::json_type_name(args)
            ));
        }
        if let Some(schema) = response_schema {
            validate_json_value(args, schema, "")
                .map_err(|err| format!("schema violation: {}", err))?;
        }
        match args.get("verdict") {
            Some(Value::String(verdict)) if !verdict.trim().is_empty() => Ok(()),
            Some(_) => Err(
                "missing verdict: the \"verdict\" key must be a non-empty string".to_string(),
            ),
            None => Err(
                "missing verdict: verdict arguments must include a string \"verdict\" key naming the stage's route"
                    .to_string(),
            ),
        }
    }

    /// Validate `question` tool arguments (architect review finding 6): parse
    /// into the canonical [`QuestionRequest`], require non-empty question
    /// text, unique option ids, and an answerable round — options present or
    /// free-form enabled. A degenerate ask would deadlock the stage waiting
    /// for an answer the user cannot give, so it is rejected here under the
    /// same poke-then-fail contract as verdict arguments.
    fn check_question_args(args: &serde_json::Value) -> Result<QuestionRequest, String> {
        let request: QuestionRequest = serde_json::from_value(args.clone()).map_err(|err| {
            format!(
                "parse error: question arguments failed to deserialize into QuestionRequest: {}",
                err
            )
        })?;
        if request.question.trim().is_empty() {
            return Err("schema violation: question text must not be empty".to_string());
        }
        let mut seen_ids = HashSet::new();
        for option in &request.options {
            let key = option.id.trim().to_lowercase();
            if !seen_ids.insert(key) {
                return Err(format!(
                    "schema violation: duplicate option id '{}'",
                    option.id
                ));
            }
        }
        if request.options.is_empty() && !request.free_form {
            return Err(
                "schema violation: degenerate question (no options and free_form=false) gives the user no way to answer; offer options and/or set free_form=true"
                    .to_string(),
            );
        }
        Ok(request)
    }

    fn json_type_name(value: &serde_json::Value) -> &'static str {
        match value {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        }
    }

    fn build_user_content(task: &TaskDefinition) -> adk_core::Content {
        let mut text = if let Some(ref prompt_text) = task.prompt_text {
            prompt_text.clone()
        } else {
            task.prompt.clone()
        };
        if !task.input_artifacts.is_null() {
            text.push_str(&format!(
                "\n\n## Input Artifacts\n```json\n{}\n```",
                serde_json::to_string_pretty(&task.input_artifacts)
                    .unwrap_or_else(|_| task.input_artifacts.to_string())
            ));
        }
        if !task.interactive {
            text.push_str(
                "\n\n## Output Contract\n\
                Your final answer MUST be a single valid JSON object — no markdown fences, \
                no commentary before or after it. The object MUST include a \"verdict\" key: \
                a string set to exactly one of the routing values documented in your stage \
                instructions (routing reads it, so any other value fails the stage). Put your \
                human-readable result under a \"content\" key and a one-line \"summary\" key \
                alongside any structured data. When your work is done, stop calling tools and \
                emit the JSON object.",
            );
        }
        adk_core::Content::new("user").with_text(text)
    }

    fn extract_text_from_content(content: &adk_core::Content) -> String {
        content
            .parts
            .iter()
            .filter_map(|p| p.text())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn extract_json(text: &str) -> Option<serde_json::Value> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return None;
        }

        if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
            return Some(v);
        }

        for content in Self::extract_code_fences(trimmed) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
                return Some(v);
            }
        }

        if let Some(json) = Self::extract_json_by_braces(trimmed)
            && let Ok(v) = serde_json::from_str::<serde_json::Value>(&json)
        {
            return Some(v);
        }

        None
    }

    fn extract_code_fences(text: &str) -> Vec<String> {
        let mut results = Vec::new();
        let mut in_fence = false;
        let mut fence_content = String::new();

        for line in text.lines() {
            let trimmed_line = line.trim();
            if trimmed_line.starts_with("```") {
                if in_fence {
                    results.push(fence_content.trim().to_string());
                    fence_content.clear();
                    in_fence = false;
                } else {
                    in_fence = true;
                }
            } else if in_fence {
                if !fence_content.is_empty() {
                    fence_content.push('\n');
                }
                fence_content.push_str(line);
            }
        }

        results
    }

    fn extract_json_by_braces(text: &str) -> Option<String> {
        let start = text.find('{')?;
        let mut depth = 0i32;
        let mut in_string = false;
        let mut escape = false;
        let bytes = text.as_bytes();

        for i in start..bytes.len() {
            let ch = bytes[i];
            if escape {
                escape = false;
                continue;
            }
            if ch == b'\\' && in_string {
                escape = true;
                continue;
            }
            if ch == b'"' {
                in_string = !in_string;
                continue;
            }
            if in_string {
                continue;
            }
            if ch == b'{' {
                depth += 1;
            } else if ch == b'}' {
                depth -= 1;
                if depth == 0 {
                    return Some(text[start..=i].to_string());
                }
            }
        }

        None
    }

    fn make_connection(session_id: &str) -> Option<RunnerConnection> {
        Some(RunnerConnection {
            host: None,
            port: None,
            password: None,
            session_id: Some(session_id.to_string()),
        })
    }

    async fn call_llm(
        &self,
        conversation: &[adk_core::Content],
        request_schema: Option<serde_json::Value>,
        tools: HashMap<String, serde_json::Value>,
        budget_scope: &str,
    ) -> Result<adk_core::Content> {
        let operation_key = format!("model_{}", uuid::Uuid::new_v4());
        self.charge_budget_with_key(tj_db::BUDGET_MODEL_CALLS, budget_scope, &operation_key)?;
        let mut request = adk_core::LlmRequest::new(&self.model_name, conversation.to_vec());

        if let Some(schema) = request_schema {
            request = request.with_response_schema(schema);
        }

        request.tools = tools;

        let tool_count = request.tools.len();
        tracing::debug!(
            model = %self.model_name,
            messages = conversation.len(),
            has_schema = request.config.as_ref().and_then(|c| c.response_schema.as_ref()).is_some(),
            tool_count = tool_count,
            "Sending LLM request via ADK"
        );

        let mut stream = self
            .model
            .generate_content(request, false)
            .await
            .context("ADK LLM generate_content failed")?;

        use futures::StreamExt;
        let mut combined_content = adk_core::Content::new("model");
        let mut had_error = false;
        let mut usage_input: Option<u64> = None;
        let mut usage_output: Option<u64> = None;

        while let Some(item) = stream.next().await {
            let response = item.context("ADK LLM response stream error")?;

            if let Some(metadata) = &response.usage_metadata {
                let input = u64::try_from(metadata.prompt_token_count.max(0)).unwrap_or(0);
                let output = u64::try_from(metadata.candidates_token_count.max(0)).unwrap_or(0);
                *usage_input.get_or_insert(0) += input;
                *usage_output.get_or_insert(0) += output;
            }

            if let Some(ref content) = response.content {
                for part in &content.parts {
                    combined_content.parts.push(part.clone());
                }
            }

            if let Some(ref err) = response.error_message
                && !err.is_empty()
            {
                tracing::error!(
                    error_code = ?response.error_code,
                    error_message = %err,
                    "LLM returned error"
                );
                had_error = true;
            }

            if response.turn_complete {
                break;
            }
        }

        let text_len = Self::extract_text_from_content(&combined_content).len();
        let func_calls = extract_function_calls(&combined_content);
        tracing::debug!(
            model = %self.model_name,
            text_len = text_len,
            function_calls = func_calls.len(),
            "LLM response received via ADK"
        );

        // Partial usage is not "known": if either side is missing the call
        // must remain explicitly unknown so a cost cap cannot silently treat
        // it as zero. Some(0) is a legitimate reported count.
        let usage = match (usage_input, usage_output) {
            (Some(input), Some(output)) => LlmUsage {
                input: Some(input),
                output: Some(output),
                reported: true,
            },
            (input, output) => LlmUsage {
                input,
                output,
                reported: false,
            },
        };
        if let Some(sink) = &self.usage {
            sink.record(budget_scope, &operation_key, &usage);
        }

        if had_error && combined_content.parts.is_empty() {
            anyhow::bail!("LLM provider reported an error with no content");
        }

        Ok(combined_content)
    }

    /// Shared tool loop behind `execute` and `send_followup`.
    ///
    /// `llm_call_timeout` carries the entry paths' timeout semantics (review
    /// fix for T-5): `Some(budget)` bounds EACH `call_llm` individually — the
    /// semantics used by interactive stages, where the blocking user wait
    /// (`question` → channel) must NOT sit inside a wall-clock deadline. The
    /// watchdog (3h20m) remains the only backstop for that wait (OQ-4, FR-19),
    /// and consecutive questions each get a fresh budget instead of sharing
    /// one. `None` imposes no internal bound — non-interactive entry paths
    /// keep their whole-loop deadline at the call site, byte-identical to the
    /// pre-T-5 behavior.
    #[allow(clippy::too_many_arguments)]
    async fn run_tool_loop(
        &self,
        initial_conversation: Vec<adk_core::Content>,
        tools: HashMap<String, serde_json::Value>,
        interactive: bool,
        response_schema: Option<serde_json::Value>,
        max_output_retries: u32,
        registry: &ToolRegistry,
        ctx: &ToolContext,
        stage_id: &str,
        stage_name: &str,
        llm_call_timeout: Option<std::time::Duration>,
    ) -> Result<ToolLoopOutcome> {
        let mut conversation = initial_conversation;
        let mut effective_tools = tools.clone();
        // Output-contract poke budget (FR-12, D-2): corrective rounds for
        // invalid verdict/question arguments within THIS loop invocation.
        // Deliberately separate from MAX_TOOL_ITERATIONS (NFR-1) and from the
        // silent-turn nudge budget below.
        let mut contract_pokes_used: u32 = 0;
        // Silent-turn nudge budget (FR-8, plan T-4): corrective followups for
        // turns that ended with NO tool call. Deliberately a SEPARATE counter
        // from `contract_pokes_used` — a stage may legitimately exhaust one
        // budget and still need the other (it can ask questions, receive
        // answers, then need format pokes, or go quiet between tool calls);
        // the two must not starve each other. Both are bounded by the ADC's
        // `max_output_retries` (D-2), and MAX_TOOL_ITERATIONS remains the
        // authoritative outer cap (NFR-1).
        let mut silent_turn_nudges: u32 = 0;

        for iteration in 0..MAX_TOOL_ITERATIONS {
            // Schema is deliberately NOT enforced during the tool loop: forced JSON
            // output on intermediate iterations suppresses tool calls. It is applied
            // on finalize/retry calls only.
            let model_content = match llm_call_timeout {
                Some(budget) => {
                    let call =
                        self.call_llm(&conversation, None, effective_tools.clone(), stage_id);
                    match tokio::time::timeout(budget, call).await {
                        Ok(content) => content?,
                        Err(_) => anyhow::bail!(
                            "LLM call timed out after {}s (per-call budget)",
                            budget.as_secs()
                        ),
                    }
                }
                None => {
                    self.call_llm(&conversation, None, effective_tools.clone(), stage_id)
                        .await?
                }
            };
            let response_text = Self::extract_text_from_content(&model_content);
            self.report(stage_id, stage_name, "llm_response", &response_text);
            conversation.push(model_content.clone());

            let function_calls = extract_function_calls(&model_content);
            if function_calls.is_empty() {
                if interactive {
                    // Silent turn (FR-8): never completes an interactive
                    // stage. Nudge the same session back into the tool
                    // contract while the budget allows; exhaustion is a
                    // visible `output contract violation:` failure — never
                    // Success, never WaitingForInput.
                    if silent_turn_nudges >= max_output_retries {
                        return Ok(ToolLoopOutcome::ContractViolation(format!(
                            "output contract violation: stage '{}' ended {} turns without a tool call",
                            stage_name,
                            silent_turn_nudges + 1
                        )));
                    }
                    if let Err(error) =
                        self.charge_budget(tj_db::BUDGET_INTERACTION_NUDGES, stage_id)
                    {
                        return Ok(ToolLoopOutcome::ContractViolation(format!(
                            "output contract violation: stage '{}' cannot be nudged: {}",
                            stage_name, error
                        )));
                    }
                    silent_turn_nudges += 1;
                    tracing::debug!(
                        iteration = iteration,
                        nudge = silent_turn_nudges,
                        "Silent turn on interactive stage, nudging back into the tool contract"
                    );
                    // FR-23(c): the nudge row names the turn and the budget.
                    self.report(
                        stage_id,
                        stage_name,
                        "nudge",
                        &format!(
                            "silent turn {}/{} ended without a tool call; nudged to ask via `question` or finish via `verdict`",
                            silent_turn_nudges, max_output_retries
                        ),
                    );
                    let nudge = format!(
                        "Nudge {}/{}: Your turn ended without a tool call. To involve the user call the `question` tool; to finish call the `verdict` tool with the required arguments. A turn without a tool call never completes the stage.",
                        silent_turn_nudges, max_output_retries
                    );
                    // One corrective round = one llm_request row (same
                    // discipline as the verdict/question poke loop).
                    self.report(stage_id, stage_name, "llm_request", &nudge);
                    conversation.push(adk_core::Content::new("user").with_text(nudge));
                    continue;
                }
                tracing::debug!(
                    iteration = iteration,
                    "No function calls in response, tool loop complete"
                );
                return Ok(ToolLoopOutcome::FinalText(response_text));
            }

            tracing::debug!(
                iteration = iteration,
                function_calls = function_calls.len(),
                "LLM requested tool calls"
            );

            let mut response_parts: Vec<adk_core::types::Part> = Vec::new();
            for (name, args, id) in &function_calls {
                tracing::debug!(
                    tool = %name,
                    call_id = ?id,
                    "Dispatching tool call"
                );
                self.report(
                    stage_id,
                    stage_name,
                    "tool_call",
                    &format!("{} {}", name, args),
                );

                let result = if interactive
                    && (name.as_str() == VERDICT_TOOL_NAME || name.as_str() == QUESTION_TOOL_NAME)
                {
                    match self
                        .resolve_interactive_tool(
                            stage_id,
                            stage_name,
                            name,
                            args,
                            response_schema.as_ref(),
                            &mut contract_pokes_used,
                            max_output_retries,
                        )
                        .await
                    {
                        InteractiveToolOutcome::Completed(artifact) => {
                            // The stage is finishing with the validated
                            // artifact; the turn's remaining calls are moot.
                            return Ok(ToolLoopOutcome::Completed(artifact));
                        }
                        InteractiveToolOutcome::Violation(reason) => {
                            return Ok(ToolLoopOutcome::ContractViolation(reason));
                        }
                        InteractiveToolOutcome::Feedback(value) => value,
                    }
                } else {
                    // Every call in a multi-call response is charged, so one
                    // model turn cannot smuggle past the tool budget.
                    self.charge_budget(tj_db::BUDGET_TOOL_CALLS, stage_id)?;
                    registry.dispatch(name, args.clone(), ctx).await
                };

                self.report(
                    stage_id,
                    stage_name,
                    "tool_result",
                    &format!("{} → {}", name, result),
                );
                response_parts.push(adk_core::types::Part::FunctionResponse {
                    function_response: adk_core::types::FunctionResponseData::new(name, result),
                    id: id.clone(),
                });
            }

            let function_response_content = adk_core::Content {
                role: "function".to_string(),
                parts: response_parts,
            };
            conversation.push(function_response_content);

            effective_tools = tools.clone();
        }

        tracing::warn!(
            iterations = MAX_TOOL_ITERATIONS,
            "Tool loop hit maximum iterations without a completion decision"
        );
        let last_model = conversation.iter().rev().find(|c| c.role == "model");
        Ok(ToolLoopOutcome::MaxToolIterations(
            last_model
                .map(Self::extract_text_from_content)
                .unwrap_or_default(),
        ))
    }

    /// Non-interactive completion gate (TJ-013): the first response gets the
    /// same local schema validation as interactive `verdict` arguments. Parse
    /// and schema errors are fed back in-session with the precise JSON path,
    /// bounded by the ADC's `max_output_retries`; exhaustion fails visibly
    /// instead of publishing an invalid artifact.
    #[allow(clippy::too_many_arguments)]
    async fn complete_noninteractive(
        &self,
        stage_id: &str,
        stage_name: &str,
        conversation: &[adk_core::Content],
        initial_response: &str,
        request_schema: Option<serde_json::Value>,
        tools: HashMap<String, serde_json::Value>,
        max_output_retries: u32,
    ) -> Result<serde_json::Value> {
        let mut last_response = initial_response.to_string();
        let mut conversation: Vec<adk_core::Content> = conversation.to_vec();
        let mut corrections: u32 = 0;

        loop {
            let failure = match Self::extract_json(&last_response) {
                None => Some("response is not a single valid JSON object".to_string()),
                Some(value) => match request_schema.as_ref() {
                    None => {
                        self.report(stage_id, stage_name, "output_validation", "ok");
                        return Ok(value);
                    }
                    Some(schema) => match validate_json_value(&value, schema, "") {
                        Ok(()) => {
                            self.report(stage_id, stage_name, "output_validation", "ok");
                            return Ok(value);
                        }
                        Err(error) => Some(format!("schema violation: {error}")),
                    },
                },
            };
            let detail = failure.expect("failure is set when no value was returned");
            self.report(stage_id, stage_name, "output_validation", &detail);
            if corrections >= max_output_retries {
                anyhow::bail!(
                    "output contract violation: stage '{}' failed validation after {} correction attempt(s); last error: {}",
                    stage_name,
                    corrections,
                    detail
                );
            }
            self.charge_budget(tj_db::BUDGET_OUTPUT_CORRECTIONS, stage_id)
                .map_err(|error| {
                    anyhow::anyhow!(
                        "output contract violation: stage '{}' cannot be corrected: {}",
                        stage_name,
                        error
                    )
                })?;
            corrections += 1;
            tracing::warn!(stage = %stage_name, correction = corrections, error = %detail, "Non-interactive output contract violation, asking LLM for a correction");
            let retry_prompt = format!(
                "output contract violation: {}. Respond with ONLY the corrected JSON object, \
                 no markdown and no commentary. Your previous response was:\n{}",
                detail, last_response
            );
            self.report(stage_id, stage_name, "llm_request", &retry_prompt);
            conversation.push(adk_core::Content::new("model").with_text(&last_response));
            conversation.push(adk_core::Content::new("user").with_text(&retry_prompt));
            last_response = self
                .call_llm(
                    &conversation,
                    request_schema.clone(),
                    tools.clone(),
                    stage_id,
                )
                .await
                .map(|content| Self::extract_text_from_content(&content))?;
        }
    }

    /// Shared interactive outcome → [`StageResult`] mapping, used identically
    /// by `execute` and `send_followup` (FR-13, FR-15): a completed decision is
    /// `Success` with the validated artifact; every non-completing outcome is
    /// a visible `output contract violation:`-prefixed failure — never
    /// `WaitingForInput`, never a silent pass.
    fn interactive_result(
        stage_id: String,
        connection: Option<RunnerConnection>,
        stage_name: &str,
        outcome: ToolLoopOutcome,
    ) -> StageResult {
        match outcome {
            ToolLoopOutcome::Completed(artifact) => StageResult {
                stage_id,
                status: StageStatus::Success,
                output: Some(artifact),
                error: None,
                connection,
            },
            ToolLoopOutcome::ContractViolation(reason) => StageResult {
                stage_id,
                status: StageStatus::Failed {
                    reason: reason.clone(),
                },
                output: None,
                error: Some(reason),
                connection,
            },
            ToolLoopOutcome::FinalText(_) => {
                let reason = format!(
                    "output contract violation: stage '{}' ended a turn without calling the verdict tool; a turn without a tool call never completes an interactive stage (FR-4, FR-9)",
                    stage_name
                );
                StageResult {
                    stage_id,
                    status: StageStatus::Failed {
                        reason: reason.clone(),
                    },
                    output: None,
                    error: Some(reason),
                    connection,
                }
            }
            ToolLoopOutcome::MaxToolIterations(_) => {
                let reason = format!(
                    "output contract violation: stage '{}' exhausted the tool-loop cap ({} iterations) without calling the verdict tool",
                    stage_name, MAX_TOOL_ITERATIONS
                );
                StageResult {
                    stage_id,
                    status: StageStatus::Failed {
                        reason: reason.clone(),
                    },
                    output: None,
                    error: Some(reason),
                    connection,
                }
            }
        }
    }
}

#[async_trait]
impl Runner for ADKRunner {
    async fn execute(&self, task: TaskDefinition) -> Result<StageResult> {
        let stage_id = task.stage_id.clone();
        let interactive = task.interactive;
        let timeout_secs = task.timeout_secs;

        tracing::debug!(
            stage_id = %stage_id,
            model = %self.model_name,
            interactive = interactive,
            timeout_secs = timeout_secs,
            has_worktree = task.worktree_path.is_some(),
            "ADKRunner executing task"
        );

        let user_content = Self::build_user_content(&task);
        let session_id = uuid::Uuid::new_v4().to_string();

        let conversation = vec![user_content.clone()];

        let output_dir = task.output_dir.clone();

        let toolset = ToolsetConfig {
            read: task.adc.tools.read,
            write: task.adc.tools.write,
            edit: task.adc.tools.edit,
            bash: task.adc.tools.bash,
        };
        let registry = ToolRegistry::new(&toolset);

        let ctx = ToolContext {
            workspace: task.worktree_path.clone().unwrap_or_default(),
            output_dir: output_dir.clone(),
        };
        let mut tool_declarations = registry.declarations(&ctx);
        if interactive {
            // Base-map merge only: effective_tools is rebuilt from this map on
            // every tool-loop iteration, so merging anywhere else drops the
            // interactive declarations from iteration 2 onward.
            tool_declarations.extend(Self::interactive_declarations(
                task.response_schema.as_ref(),
            ));
        }
        let has_tools = !tool_declarations.is_empty();

        tracing::debug!(
            stage_id = %stage_id,
            tool_count = tool_declarations.len(),
            has_tools = has_tools,
            "Tool declarations prepared"
        );

        self.report(
            &stage_id,
            &task.adc.id,
            "llm_request",
            &Self::extract_text_from_content(&user_content),
        );

        let stage_name = task.adc.id.clone();
        // The session is registered BEFORE the tool loop starts (review fix
        // for T-5): a per-call LLM timeout no longer erases the conversation —
        // post-mortem inspection and late channel answers keep their context.
        // Insert is mutex-scoped; no lock is held across the awaits below.
        let session = Arc::new(RunnerSession::new(
            session_id.clone(),
            stage_id.clone(),
            stage_name.clone(),
            Some(toolset),
            task.worktree_path.clone().unwrap_or_default(),
            output_dir.clone(),
            interactive,
            task.response_schema.clone(),
            task.adc.max_output_retries,
            timeout_secs,
        ));
        session.append(user_content);
        self.sessions
            .lock()
            .unwrap()
            .insert(session_id.clone(), Arc::clone(&session));

        let llm_call_timeout = interactive.then(|| std::time::Duration::from_secs(timeout_secs));
        let loop_call = self.run_tool_loop(
            conversation,
            tool_declarations,
            interactive,
            task.response_schema.clone(),
            task.adc.max_output_retries,
            &registry,
            &ctx,
            &stage_id,
            &task.adc.id,
            llm_call_timeout,
        );
        let outcome = if interactive {
            // Interactive (review fix for T-5): NO whole-loop wall-clock
            // deadline — the blocking user wait (`question` → channel) must
            // not race `timeout_secs`, or a slow answer drops the future
            // mid-receive and fails the stage. Each LLM call carries its own
            // budget instead (see `run_tool_loop`); the watchdog (3h20m) is
            // the only backstop for the wait itself (OQ-4, FR-19), and
            // consecutive questions never share a budget. `send_followup`
            // mirrors these semantics exactly.
            Ok(loop_call.await)
        } else {
            // Non-interactive: unchanged whole-loop deadline (byte-identical
            // to the pre-T-5 behavior).
            tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), loop_call).await
        };

        let outcome = match outcome {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(e)) => {
                tracing::error!(stage_id = %stage_id, error = %e, "LLM call failed");
                return Ok(StageResult {
                    stage_id,
                    status: StageStatus::Failed {
                        reason: format!("LLM call failed: {}", e),
                    },
                    output: None,
                    error: Some(e.to_string()),
                    connection: None,
                });
            }
            Err(_) => {
                tracing::error!(
                    stage_id = %stage_id,
                    timeout_secs = timeout_secs,
                    "LLM call timed out"
                );
                return Ok(StageResult {
                    stage_id,
                    status: StageStatus::Timeout,
                    output: None,
                    error: Some(format!("Timed out after {}s", timeout_secs)),
                    connection: None,
                });
            }
        };

        // Record the loop's transcript; the session itself was registered
        // before the loop started.
        session.append(adk_core::Content::new("model").with_text(&outcome.transcript_text()));

        if interactive {
            // Single completion decision (FR-4): the loop's outcome already
            // carries the validated verdict; the mapping is shared with
            // `send_followup`.
            Ok(Self::interactive_result(
                stage_id,
                Self::make_connection(&session_id),
                &stage_name,
                outcome,
            ))
        } else {
            // Non-interactive channel (FR-10), behavior-identical: both
            // non-completing outcomes surface the loop's final text, which
            // flows through extract_json → execute_with_retry exactly as
            // before. Interactive outcomes cannot occur here (the dispatch
            // site gates resolution on `interactive`).
            let response_text = match outcome {
                ToolLoopOutcome::FinalText(text) | ToolLoopOutcome::MaxToolIterations(text) => text,
                ToolLoopOutcome::Completed(_) | ToolLoopOutcome::ContractViolation(_) => {
                    unreachable!("non-interactive tool loops never produce interactive outcomes")
                }
            };

            let snapshot = {
                let sessions = self.sessions.lock().unwrap();
                sessions
                    .get(&session_id)
                    .map(|s| s.conversation())
                    .unwrap_or_default()
            };
            let output = self
                .complete_noninteractive(
                    &session.stage_id,
                    &session.stage_name,
                    &snapshot,
                    &response_text,
                    task.response_schema.clone(),
                    HashMap::new(),
                    task.adc.max_output_retries,
                )
                .await?;

            Ok(StageResult {
                stage_id,
                status: StageStatus::Success,
                output: Some(output),
                error: None,
                connection: Self::make_connection(&session_id),
            })
        }
    }

    async fn send_followup(&self, session_id: &str, message: &str) -> Result<StageResult> {
        tracing::debug!(
            session_id = %session_id,
            message_len = message.len(),
            "ADKRunner sending followup"
        );

        let session = {
            let sessions = self.sessions.lock().unwrap();
            match sessions.get(session_id) {
                Some(s) => Arc::clone(s),
                None => {
                    anyhow::bail!("Session not found: {}", session_id);
                }
            }
        };

        let (registry, ctx) = match &session.toolset_config {
            Some(toolset) => {
                tracing::debug!(
                    session_id = %session_id,
                    "Reconstructing tool registry for followup"
                );
                let registry = ToolRegistry::new(toolset);
                let ctx = ToolContext {
                    workspace: session.workspace.clone(),
                    output_dir: session.output_dir.clone(),
                };
                (Some(registry), Some(ctx))
            }
            None => {
                tracing::debug!(
                    session_id = %session_id,
                    "No toolset config stored in session, followup without tools"
                );
                (None, None)
            }
        };

        session.append(adk_core::Content::new("user").with_text(message));

        let conversation = session.conversation();
        self.report(
            &session.stage_id,
            &session.stage_name,
            "llm_request",
            message,
        );

        // Same timeout semantics as `execute` (review fix for T-5): an
        // interactive loop bounds EACH LLM call and never budgets the
        // blocking user wait (watchdog is the backstop, OQ-4); a
        // non-interactive loop keeps the whole-loop deadline. Previously this
        // path had NO timeout at all — the execute-vs-followup inconsistency
        // the T-5 review flagged.
        let interactive = session.interactive;
        let timeout_secs = session.timeout_secs;
        let llm_call_timeout = interactive.then(|| std::time::Duration::from_secs(timeout_secs));
        let loop_call = async {
            match (registry, ctx) {
                (Some(reg), Some(ctxt)) => {
                    let mut tool_declarations = reg.declarations(&ctxt);
                    if interactive {
                        // Same base-map merge discipline as `execute`.
                        tool_declarations.extend(Self::interactive_declarations(
                            session.response_schema.as_ref(),
                        ));
                    }
                    tracing::debug!(
                        session_id = %session_id,
                        tool_count = tool_declarations.len(),
                        "Running tool loop for followup"
                    );
                    self.run_tool_loop(
                        conversation,
                        tool_declarations,
                        interactive,
                        session.response_schema.clone(),
                        session.max_output_retries,
                        &reg,
                        &ctxt,
                        &session.stage_id,
                        &session.stage_name,
                        llm_call_timeout,
                    )
                    .await
                }
                _ => {
                    tracing::debug!(session_id = %session_id, "Calling LLM without tools for followup");
                    let text = self
                        .call_llm(&conversation, None, HashMap::new(), &session.stage_id)
                        .await
                        .map(|c| Self::extract_text_from_content(&c))?;
                    Ok(ToolLoopOutcome::FinalText(text))
                }
            }
        };
        let outcome = if interactive {
            loop_call.await?
        } else {
            match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), loop_call)
                .await
            {
                Ok(result) => result?,
                Err(_) => anyhow::bail!(
                    "followup LLM work timed out after {}s (whole-loop budget)",
                    timeout_secs
                ),
            }
        };

        session.append(adk_core::Content::new("model").with_text(&outcome.transcript_text()));

        if session.interactive {
            // Same single completion decision as `execute` (FR-14): a verdict
            // call on a followup turn validates and completes identically.
            Ok(Self::interactive_result(
                String::new(),
                Self::make_connection(session_id),
                &session.stage_name,
                outcome,
            ))
        } else {
            tracing::debug!(
                session_id = %session_id,
                "Followup waiting for more user input"
            );
            let output = serde_json::Value::Object({
                let mut map = serde_json::Map::new();
                map.insert(
                    "message".into(),
                    serde_json::Value::String(outcome.transcript_text()),
                );
                map
            });
            Ok(StageResult {
                stage_id: String::new(),
                status: StageStatus::WaitingForInput,
                output: Some(output),
                error: None,
                connection: Self::make_connection(session_id),
            })
        }
    }
}
