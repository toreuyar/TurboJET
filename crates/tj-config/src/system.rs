use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurboJetConfig {
    #[serde(default = "default_listen_addr")]
    pub listen_addr: String,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default = "default_database_path")]
    pub database_path: String,
    #[serde(default = "default_workspace_base")]
    pub workspace_base: String,
    /// Preferred explicit workflow sources. Empty or absent is valid: the
    /// service starts with an empty registry and reports degraded readiness.
    #[serde(default)]
    pub workflow_sources: Vec<WorkflowSource>,
    /// Deprecated single-file alias for `workflow_sources`. There is no
    /// built-in default path; an explicitly configured missing file fails
    /// startup rather than being ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smg_path: Option<String>,
    /// Legacy shared ADC directory used only by bare `agent` ids in `file` or
    /// `directory` graphs. Empty by default: there is no built-in shared agent
    /// tree, and a bare reference without an explicit directory fails with an
    /// actionable error. Package workflows never consult this field.
    #[serde(default)]
    pub adc_dir: String,
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    #[serde(default)]
    pub agents: Vec<AgentConfig>,
    #[serde(default)]
    pub chat: Option<ChatConfig>,
    /// Optional absolute wall-clock limit for new runs, including human waits
    /// and gates. Omitted preserves the existing per-attempt watchdog policy.
    #[serde(default)]
    pub run_timeout_secs: Option<u64>,
    /// D5/TJ-016 run-wide question, model, tool, correction and active-time
    /// limits. Frozen into every new execution bundle; omitted uses defaults.
    #[serde(default)]
    pub budgets: crate::budget::RunBudgetLimits,
    /// Bounded retention for durable stage activity (model/tool traffic and
    /// provider attempts). 0 disables automatic pruning. Runs, artifacts,
    /// decisions and packets are never pruned by this setting.
    #[serde(default = "default_activity_retention_days")]
    pub activity_retention_days: u64,
    /// Optional external CI/CD integration (TJ-039). Omitted disables delivery
    /// dispatch while preserving durable history and inspection routes.
    #[serde(default)]
    pub delivery: Option<DeliveryConfig>,
}

/// Forgejo-Actions-compatible delivery adapter. TurboJet submits a workflow
/// dispatch request and verifies signed callbacks or polls runs; it never
/// performs the deployment itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryConfig {
    #[serde(default = "default_delivery_provider")]
    pub provider: String,
    pub base_url: String,
    pub token_env: String,
    pub owner: String,
    pub repo: String,
    pub workflow: String,
    #[serde(default = "default_delivery_ref")]
    pub git_ref: String,
    pub callback_secret_env: String,
    #[serde(default = "default_callback_window_secs")]
    pub callback_window_secs: i64,
    #[serde(default)]
    pub callback_base_url: Option<String>,
}

fn default_delivery_provider() -> String {
    "forgejo_actions".into()
}
fn default_delivery_ref() -> String {
    "main".into()
}
fn default_callback_window_secs() -> i64 {
    300
}

fn default_listen_addr() -> String {
    "127.0.0.1:8100".into()
}
fn default_log_level() -> String {
    if cfg!(debug_assertions) {
        "debug".into()
    } else {
        "info".into()
    }
}
fn default_database_path() -> String {
    "$TURBOJET_HOME/state.db".into()
}
fn default_workspace_base() -> String {
    ".turbojet".into()
}
fn default_activity_retention_days() -> u64 {
    30
}

/// One explicit workflow graph source. Directory sources are scanned
/// non-recursively in sorted path order for `*.smg.json`; nested directories
/// must be listed explicitly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowSource {
    /// Stable operator id. Defaults to a slug of the path; duplicates fail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Absolute or base-dir-relative path.
    pub path: String,
    pub kind: WorkflowSourceKind,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowSourceKind {
    File,
    Directory,
    /// One self-contained package directory. Import stores the graph and every
    /// referenced asset in the registry; admission never rereads the source.
    Package,
}

impl WorkflowSourceKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
            Self::Package => "package",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    pub id: String,
    #[serde(rename = "type")]
    pub provider_type: ProviderType,
    pub api_key_env: String,
    #[serde(default)]
    pub base_url: String,
    pub default_model: String,
    /// Retry attempts allowed after a provider rate-limit response (HTTP 429).
    /// Each class has its own budget; tool calls are never replayed to retry
    /// transport.
    #[serde(default = "default_rate_limit_retries")]
    pub rate_limit_retries: u32,
    /// Retry attempts for transient transport/provider failures (timeouts,
    /// connection resets, HTTP 408/5xx). Credential errors never retry.
    #[serde(default = "default_transient_retries")]
    pub transient_retries: u32,
    /// Base exponential backoff in milliseconds for provider retries.
    #[serde(default = "default_retry_base_ms")]
    pub retry_base_ms: u64,
    /// Optional monetary accounting. Zero means "price unknown", so cost is
    /// reported as unknown rather than guessed.
    #[serde(default)]
    pub input_cost_micros_per_million: u64,
    #[serde(default)]
    pub output_cost_micros_per_million: u64,
}

fn default_rate_limit_retries() -> u32 {
    3
}
fn default_transient_retries() -> u32 {
    2
}
fn default_retry_base_ms() -> u64 {
    250
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderType {
    Anthropic,
    OpenAi,
    Gemini,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    pub id: String,
    #[serde(default)]
    pub provider_id: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub default: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            provider_id: String::new(),
            model: String::new(),
            system_prompt: String::new(),
            tools: Vec::new(),
            default: false,
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatConfig {
    pub provider: String,
    #[serde(default)]
    pub zulip: Option<ZulipChatConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ZulipChatConfig {
    pub server: String,
    pub bot_email: String,
    pub api_key_env: String,
    #[serde(default)]
    pub default_stream: String,
}
