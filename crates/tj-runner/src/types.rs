use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tj_config::adc::AgentDefinitionConfig;
use tj_db::RunnerConnection;

/// One budgeted operation. Implementations must be durable and idempotent for
/// `operation_key`: a transport retry with the same key is a replay, while a
/// genuinely new model/tool/question call gets a new key and consumes budget.
pub trait BudgetGuard: Send + Sync {
    fn charge(&self, kind: &str, scope: &str, operation_key: &str) -> Result<()>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDefinition {
    pub stage_id: String,
    pub job_id: String,
    /// Run identity used by the attached budget guard.
    #[serde(default)]
    pub run_id: String,
    pub prompt: String,
    pub prompt_text: Option<String>,
    pub input_artifacts: serde_json::Value,
    pub output_dir: PathBuf,
    pub output_file: Option<String>,
    pub timeout_secs: u64,
    pub adc: AgentDefinitionConfig,
    pub runner_type: String,
    pub worktree_path: Option<PathBuf>,
    pub interactive: bool,
    /// Optional JSON schema the stage's final answer must satisfy (from the ADC).
    #[serde(default)]
    pub response_schema: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageResult {
    pub stage_id: String,
    pub status: StageStatus,
    pub output: Option<serde_json::Value>,
    pub error: Option<String>,
    pub connection: Option<RunnerConnection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum StageStatus {
    Success,
    Failed { reason: String },
    Timeout,
    WaitingForInput,
}

/// One multiple-choice option inside a [`QuestionRequest`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub id: String,
    pub label: String,
}

/// Structured payload of the interactive `question` tool: the single source of
/// truth mirrored by the webui and Zulip renderers (REQ FR-6). A round may be
/// multi-select only (`multi_select`, empty `free_form`), free-text only
/// (empty `options`, `free_form`), or both at once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestionRequest {
    pub question: String,
    /// Defaults to empty so a free-text-only ask (`free_form: true`) may omit
    /// it, matching the `question`-only required set in the tool declaration.
    #[serde(default)]
    pub options: Vec<QuestionOption>,
    #[serde(default)]
    pub multi_select: bool,
    #[serde(default)]
    pub free_form: bool,
}

/// Structured answer returned for a [`QuestionRequest`]. `selected` carries
/// option ids (parsers may additionally accept label text case-insensitively);
/// `text` carries the free-form reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestionAnswer {
    #[serde(default)]
    pub selected: Vec<String>,
    #[serde(default)]
    pub text: Option<String>,
}

/// Blocking input channel for interactive stages: delivers a [`QuestionRequest`]
/// to the user (webui ∥ Zulip via the stage-side implementation) and returns the
/// parsed [`QuestionAnswer`]. Implemented by tj-stage; attached to the runner
/// with [`crate::adk_runner::ADKRunner::with_question_channel`].
#[async_trait]
pub trait QuestionChannel: Send + Sync {
    async fn ask(
        &self,
        stage_id: &str,
        stage_name: &str,
        request: &QuestionRequest,
    ) -> Result<QuestionAnswer>;
}

/// Receives structured activity from a stage execution (LLM requests/responses,
/// tool calls and results) so runs can be replayed and live-streamed.
pub trait StageLogSink: Send + Sync {
    fn log(&self, stage_id: &str, stage_name: &str, kind: &str, content: &str);
}

/// Token usage observed for one model call. `reported == false` means the
/// provider omitted usage; it is explicitly unknown, never measured zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LlmUsage {
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub reported: bool,
}

impl LlmUsage {
    pub fn unknown() -> Self {
        Self {
            input: None,
            output: None,
            reported: false,
        }
    }
}

/// Records durable token/cost accounting for a model call. The operation key
/// is the same key used for the durable call-budget charge, so replaying the
/// same transport operation cannot double count.
pub trait UsageSink: Send + Sync {
    fn record(&self, stage_id: &str, operation_key: &str, usage: &LlmUsage);
}

#[async_trait]
pub trait Runner: Send + Sync {
    async fn execute(&self, task: TaskDefinition) -> Result<StageResult>;
    async fn send_followup(&self, session_id: &str, message: &str) -> Result<StageResult>;
}
