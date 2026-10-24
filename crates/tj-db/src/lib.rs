use anyhow::{Context, Result};
pub use process_lock::{DatabaseProcessLock, acquire_for_database, try_acquire_for_database};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Mutex;

macro_rules! define_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name(pub String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_string())
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

define_id!(RunId);
define_id!(FlowId);
define_id!(StageId);
define_id!(ProjectId);
define_id!(LaneId);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationRecord {
    pub id: String,
    pub run_id: RunId,
    pub conversation_type: String,
    pub provider: String,
    pub provider_key: String,
    pub stage_name: Option<String>,
    pub session_id: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub username: String,
    pub password_hash: String,
    pub display_name: Option<String>,
    pub is_admin: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateDecision {
    pub id: String,
    pub run_id: RunId,
    pub gate_name: String,
    pub actor_id: String,
    pub actor_name: String,
    pub approved: bool,
    pub reason: Option<String>,
    pub note: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub gate_instance_id: Option<String>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub expected_state_version: Option<i64>,
    #[serde(default)]
    pub state_version: Option<i64>,
    #[serde(default)]
    pub disposition: Option<String>,
    /// Exact packet content the reviewer saw (TJ-021). Required when the
    /// active gate has a packet binding.
    #[serde(default)]
    pub packet_revision_id: Option<String>,
    #[serde(default)]
    pub packet_content_hash: Option<String>,
    /// Exact structured plan revision/hash under review (TJ-032). Required
    /// when the active gate is plan-bound; always paired with the source
    /// packet identity above.
    #[serde(default)]
    pub plan_revision_id: Option<String>,
    #[serde(default)]
    pub plan_content_hash: Option<String>,
    /// Exact generic output artifact under review, when the gate is not
    /// packet/plan bound. The artifact ID/version/hash are immutable.
    #[serde(default)]
    pub output_artifact_id: Option<String>,
    #[serde(default)]
    pub output_content_hash: Option<String>,
    #[serde(default)]
    pub output_stage_name: Option<String>,
    /// Explicit acknowledgement that an incomplete packet's open
    /// unknowns/decisions were reviewed. Interactive UI can set this; Chief
    /// cannot silently approve an incomplete packet.
    #[serde(default)]
    pub acknowledge_incomplete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GateDecisionReceipt {
    pub id: String,
    pub run_id: String,
    pub gate_instance_id: String,
    pub approved: bool,
    pub status: RunStatus,
    pub state_version: i64,
    pub disposition: String,
    pub replayed: bool,
    /// Exact packet reviewed by this command, when the gate is packet-bound.
    /// Carried in the receipt so a client does not need a second read to
    /// confirm what content it approved or rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packet_revision_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packet_content_hash: Option<String>,
    /// Exact structured plan reviewed by this command, when plan-bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_revision_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_content_hash: Option<String>,
    /// Exact generic output artifact reviewed by this command, when bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_artifact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_content_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_stage_name: Option<String>,
    /// Normalized review identity kind from the durable binding rows:
    /// `packet`, `plan` or `artifact`. Absent only for an old unbound gate.
    /// Existing receipt fields above remain authoritative and unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_binding_kind: Option<String>,
    /// Durable routing effect accepted with this decision. Managers reconcile
    /// this row instead of guessing whether the route ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunEvent {
    pub id: String,
    pub run_id: RunId,
    pub kind: String,
    pub actor: Option<String>,
    pub flow_name: Option<String>,
    pub stage_name: Option<String>,
    pub summary: Option<String>,
    pub payload: Option<String>,
    pub created_at: String,
}

/// One captured step of a stage's execution (LLM traffic, tool calls, notes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageLog {
    pub id: i64,
    pub run_id: RunId,
    pub stage_id: String,
    pub stage_name: Option<String>,
    pub flow_name: Option<String>,
    pub kind: String,
    pub content: String,
    pub created_at: String,
}

/// Immutable, versioned artifact row (append-only; fork = new version).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunArtifact {
    pub id: String,
    pub run_id: RunId,
    pub flow_name: String,
    pub stage_name: String,
    pub attempt: i64,
    pub version: i64,
    pub content: String,
    pub content_hash: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub repo_url: Option<String>,
    pub base_branch: String,
    pub workspace_path: Option<String>,
    pub git_provider: Option<String>,
    /// Legacy display identity only. New projects use `catalog_id`; behavior is
    /// derived from the pinned workflow contract, never this field.
    #[serde(default = "default_project_profile")]
    pub profile: String,
    /// Derived workspace policy for new projects: `none` or `repository`.
    /// Legacy rows leave this NULL and use their frozen profile behavior.
    #[serde(default)]
    pub workspace_kind: Option<String>,
    /// Registry catalog identity selected at project creation.
    #[serde(default)]
    pub catalog_id: Option<String>,
    /// `ready`, `provisioning`, `failed` or `not_required`. Remote clone
    /// failures are recorded here instead of substituting an empty repository.
    #[serde(default)]
    pub provisioning_state: Option<String>,
    #[serde(default)]
    pub provisioning_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

pub fn default_project_profile() -> String {
    "full".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lane {
    pub id: String,
    pub project_id: ProjectId,
    pub name: String,
    pub smg_path: String,
    pub description: Option<String>,
    pub workflow_id: Option<String>,
    pub created_at: String,
}

/// An immutable workflow (SMG) version in the registry. Content-addressed:
/// re-importing identical content is a no-op, so versions never change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowVersion {
    pub id: String,
    pub name: String,
    pub smg_path: String,
    pub version: i64,
    pub revision: i64,
    pub description: Option<String>,
    pub content_hash: String,
    pub content_json: String,
    pub entry: Option<String>,
    pub exit: Option<String>,
    pub flow_count: i64,
    pub gate_count: i64,
    pub stage_count: i64,
    /// Catalog metadata parsed from the imported graph. `content_json` and
    /// `content_hash` remain authoritative.
    #[serde(default)]
    pub catalog_id: Option<String>,
    #[serde(default)]
    pub catalog_label: Option<String>,
    #[serde(default)]
    pub catalog_description: Option<String>,
    #[serde(default)]
    pub catalog_default: bool,
    /// 0 means a pre-catalog row; it is historical/read-only and cannot admit a
    /// new run. New imports set 1 even when only generic defaults apply.
    #[serde(default)]
    pub capability_schema_version: i64,
    #[serde(default)]
    pub capability_hash: Option<String>,
    #[serde(default)]
    pub source_id: Option<String>,
    #[serde(default)]
    pub source_rel_path: Option<String>,
    /// `file`, `directory` or `package`. Historical rows have no kind.
    #[serde(default)]
    pub source_kind: Option<String>,
    pub created_at: String,
}

/// One immutable text asset belonging to a workflow version. Assets exist for
/// package workflows; legacy graph-only imports have no rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowAsset {
    pub logical_path: String,
    pub content_hash: String,
    pub content: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionBundleRecord {
    pub id: String,
    pub run_id: RunId,
    pub workflow_id: Option<String>,
    pub bundle_hash: String,
    pub bundle_json: String,
    /// Explicit marker for rows that predate bundles. Legacy rows never claim
    /// full reproducibility.
    pub legacy: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineRun {
    #[serde(
        rename = "state_version",
        serialize_with = "lifecycle::serialize_revision",
        skip_deserializing
    )]
    revision: lifecycle::Revision,
    id: String,
    project_id: Option<ProjectId>,
    lane_id: Option<LaneId>,
    workflow_id: Option<String>,
    /// Immutable execution bundle created at admission (TJ-012). `None` marks
    /// a legacy run admitted before bundles existed; it cannot claim full
    /// reproducibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    execution_bundle_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    execution_bundle_hash: Option<String>,
    description: String,
    status: RunStatus,
    current_flow_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    deadline_at: Option<String>,
    gate_state: Option<String>,
    gate_instance_id: Option<String>,
    gate_name: Option<String>,
    repo_url: Option<String>,
    base_branch: Option<String>,
    trace_id: Option<String>,
    slug: Option<String>,
    pr_url: Option<String>,
    pr_number: Option<u64>,
    created_at: String,
    updated_at: String,
}

impl PipelineRun {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: String,
        project_id: Option<ProjectId>,
        lane_id: Option<LaneId>,
        description: String,
        status: RunStatus,
        repo_url: Option<String>,
        base_branch: Option<String>,
        trace_id: Option<String>,
        slug: Option<String>,
    ) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id,
            project_id,
            revision: lifecycle::Revision::default(),
            lane_id,
            workflow_id: None,
            execution_bundle_id: None,
            execution_bundle_hash: None,
            description,
            status,
            current_flow_id: None,
            deadline_at: None,
            gate_state: None,
            gate_instance_id: None,
            gate_name: None,
            repo_url,
            base_branch,
            trace_id,
            slug,
            pr_url: None,
            pr_number: None,
            created_at: now.clone(),
            updated_at: now,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn project_id(&self) -> Option<&ProjectId> {
        self.project_id.as_ref()
    }
    pub fn lane_id(&self) -> Option<&LaneId> {
        self.lane_id.as_ref()
    }
    pub fn workflow_id(&self) -> Option<&str> {
        self.workflow_id.as_deref()
    }
    pub fn execution_bundle_id(&self) -> Option<&str> {
        self.execution_bundle_id.as_deref()
    }
    pub fn execution_bundle_hash(&self) -> Option<&str> {
        self.execution_bundle_hash.as_deref()
    }
    pub fn description(&self) -> &str {
        &self.description
    }
    pub fn status(&self) -> &RunStatus {
        &self.status
    }
    pub fn current_flow_id(&self) -> Option<&str> {
        self.current_flow_id.as_deref()
    }
    pub fn deadline_at(&self) -> Option<&str> {
        self.deadline_at.as_deref()
    }
    pub fn set_deadline_at(&mut self, deadline: Option<String>) {
        self.deadline_at = deadline;
    }
    pub fn gate_state(&self) -> Option<&str> {
        self.gate_state.as_deref()
    }
    pub fn gate_instance_id(&self) -> Option<&str> {
        self.gate_instance_id.as_deref()
    }
    pub fn gate_name(&self) -> Option<&str> {
        self.gate_name.as_deref()
    }
    pub fn state_version(&self) -> i64 {
        self.revision.get()
    }
    pub fn repo_url(&self) -> Option<&str> {
        self.repo_url.as_deref()
    }
    pub fn base_branch(&self) -> Option<&str> {
        self.base_branch.as_deref()
    }
    pub fn trace_id(&self) -> Option<&str> {
        self.trace_id.as_deref()
    }
    pub fn created_at(&self) -> &str {
        &self.created_at
    }
    pub fn updated_at(&self) -> &str {
        &self.updated_at
    }

    pub fn slug(&self) -> Option<&str> {
        self.slug.as_deref()
    }
    pub fn pr_url(&self) -> Option<&str> {
        self.pr_url.as_deref()
    }
    pub fn pr_number(&self) -> Option<u64> {
        self.pr_number
    }

    pub fn set_workflow_id(&mut self, workflow_id: Option<String>) {
        self.workflow_id = workflow_id;
    }
    pub fn set_execution_bundle(&mut self, id: Option<String>, hash: Option<String>) {
        self.execution_bundle_id = id;
        self.execution_bundle_hash = hash;
    }
    pub fn set_status(&mut self, status: RunStatus) {
        self.status = status;
    }
    pub fn set_gate_state(&mut self, state: Option<String>) {
        self.gate_state = state;
    }
    pub fn set_gate_identity(&mut self, id: Option<String>, name: Option<String>) {
        self.gate_instance_id = id;
        self.gate_name = name;
    }
    pub fn set_updated_at(&mut self, ts: String) {
        self.updated_at = ts;
    }
    pub fn set_repo_url(&mut self, url: Option<String>) {
        self.repo_url = url;
    }
    pub fn set_base_branch(&mut self, branch: Option<String>) {
        self.base_branch = branch;
    }
    pub fn set_slug(&mut self, slug: Option<String>) {
        self.slug = slug;
    }
    pub fn set_pr_url(&mut self, url: Option<String>) {
        self.pr_url = url;
    }
    pub fn set_pr_number(&mut self, number: Option<u64>) {
        self.pr_number = number;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    WaitingForGate,
    /// The previous process stopped while work may have been in flight. No
    /// automatic replay occurs; an explicit retry starts a new attempt.
    Interrupted,
    Completed,
    Failed,
    Cancelled,
}

impl std::fmt::Display for RunStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunStatus::Running => write!(f, "running"),
            RunStatus::WaitingForGate => write!(f, "waiting_for_gate"),
            RunStatus::Interrupted => write!(f, "interrupted"),
            RunStatus::Completed => write!(f, "completed"),
            RunStatus::Failed => write!(f, "failed"),
            RunStatus::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl FromStr for RunStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "running" => Ok(RunStatus::Running),
            "waiting_for_gate" | "WaitingForGate" => Ok(RunStatus::WaitingForGate),
            "interrupted" | "Interrupted" => Ok(RunStatus::Interrupted),
            "completed" => Ok(RunStatus::Completed),
            "failed" => Ok(RunStatus::Failed),
            "cancelled" => Ok(RunStatus::Cancelled),
            _ => Err(format!("Unknown RunStatus: {}", s)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PipelineNotification {
    FlowCompleted {
        run_id: String,
        flow: String,
        summary: String,
    },
    GateReached {
        run_id: String,
        flow: String,
        summary: String,
    },
    StageCompleted {
        run_id: String,
        stage: String,
        summary: String,
    },
    StageWaitingForInput {
        run_id: String,
        stage: String,
        message: String,
    },
    PipelineCompleted {
        run_id: String,
        summary: String,
    },
    PipelineFailed {
        run_id: String,
        reason: String,
        user_message: Option<String>,
    },
}

impl PipelineNotification {
    pub fn run_id(&self) -> &str {
        match self {
            PipelineNotification::FlowCompleted { run_id, .. } => run_id,
            PipelineNotification::GateReached { run_id, .. } => run_id,
            PipelineNotification::StageCompleted { run_id, .. } => run_id,
            PipelineNotification::StageWaitingForInput { run_id, .. } => run_id,
            PipelineNotification::PipelineCompleted { run_id, .. } => run_id,
            PipelineNotification::PipelineFailed { run_id, .. } => run_id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowRecord {
    #[serde(skip)]
    revision: lifecycle::Revision,
    id: String,
    run_id: RunId,
    flow_name: String,
    flow_number: Option<u64>,
    status: FlowStatus,
    current_stage_id: Option<String>,
    worktree_path: Option<String>,
    branch_name: Option<String>,
    created_at: String,
    started_at: Option<String>,
    completed_at: Option<String>,
}

impl FlowRecord {
    pub fn new(
        id: String,
        run_id: RunId,
        flow_name: String,
        flow_number: Option<u64>,
        worktree_path: Option<String>,
        branch_name: Option<String>,
    ) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id,
            run_id,
            revision: lifecycle::Revision::default(),
            flow_name,
            flow_number,
            status: FlowStatus::Running,
            current_stage_id: None,
            worktree_path,
            branch_name,
            created_at: now.clone(),
            started_at: Some(now),
            completed_at: None,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }
    pub fn flow_name(&self) -> &str {
        &self.flow_name
    }
    pub fn flow_number(&self) -> Option<u64> {
        self.flow_number
    }
    pub fn status(&self) -> &FlowStatus {
        &self.status
    }
    pub fn current_stage_id(&self) -> Option<&str> {
        self.current_stage_id.as_deref()
    }
    pub fn worktree_path(&self) -> Option<&str> {
        self.worktree_path.as_deref()
    }
    pub fn branch_name(&self) -> Option<&str> {
        self.branch_name.as_deref()
    }
    pub fn created_at(&self) -> &str {
        &self.created_at
    }
    pub fn started_at(&self) -> Option<&str> {
        self.started_at.as_deref()
    }
    pub fn completed_at(&self) -> Option<&str> {
        self.completed_at.as_deref()
    }

    pub fn set_status(&mut self, status: FlowStatus) {
        self.status = status;
    }
    pub fn set_completed_at(&mut self, ts: Option<String>) {
        self.completed_at = ts;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FlowStatus {
    Pending,
    Running,
    Completed,
    Failed,
    TimedOut,
}

impl std::fmt::Display for FlowStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FlowStatus::Pending => write!(f, "pending"),
            FlowStatus::Running => write!(f, "running"),
            FlowStatus::Completed => write!(f, "completed"),
            FlowStatus::Failed => write!(f, "failed"),
            FlowStatus::TimedOut => write!(f, "timed_out"),
        }
    }
}

impl FromStr for FlowStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(FlowStatus::Pending),
            "running" => Ok(FlowStatus::Running),
            "completed" => Ok(FlowStatus::Completed),
            "failed" => Ok(FlowStatus::Failed),
            "timed_out" | "TimedOut" => Ok(FlowStatus::TimedOut),
            _ => Err(format!("Unknown FlowStatus: {}", s)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerConnection {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub password: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageRecord {
    #[serde(skip)]
    revision: lifecycle::Revision,
    id: String,
    flow_id: FlowId,
    stage_name: String,
    status: StageStatus,
    input_artifacts: Option<String>,
    output_artifacts: Option<String>,
    error: Option<String>,
    iteration: u32,
    created_at: String,
    started_at: Option<String>,
    completed_at: Option<String>,
    connection: Option<RunnerConnection>,
}

impl StageRecord {
    pub fn new(
        id: String,
        flow_id: FlowId,
        stage_name: String,
        status: StageStatus,
        iteration: u32,
        input_artifacts: Option<String>,
    ) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id,
            flow_id,
            revision: lifecycle::Revision::default(),
            stage_name,
            status,
            input_artifacts,
            output_artifacts: None,
            error: None,
            iteration,
            created_at: now.clone(),
            started_at: Some(now),
            completed_at: None,
            connection: None,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn flow_id(&self) -> &FlowId {
        &self.flow_id
    }
    pub fn stage_name(&self) -> &str {
        &self.stage_name
    }
    pub fn status(&self) -> &StageStatus {
        &self.status
    }
    pub fn input_artifacts(&self) -> Option<&str> {
        self.input_artifacts.as_deref()
    }
    pub fn output_artifacts(&self) -> Option<&str> {
        self.output_artifacts.as_deref()
    }
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
    pub fn iteration(&self) -> u32 {
        self.iteration
    }
    pub fn created_at(&self) -> &str {
        &self.created_at
    }
    pub fn started_at(&self) -> Option<&str> {
        self.started_at.as_deref()
    }
    pub fn completed_at(&self) -> Option<&str> {
        self.completed_at.as_deref()
    }
    pub fn connection(&self) -> Option<&RunnerConnection> {
        self.connection.as_ref()
    }

    pub fn set_status(&mut self, status: StageStatus) {
        self.status = status;
    }
    pub fn set_output_artifacts(&mut self, artifacts: Option<String>) {
        self.output_artifacts = artifacts;
    }
    pub fn set_error(&mut self, error: Option<String>) {
        self.error = error;
    }
    pub fn set_completed_at(&mut self, ts: Option<String>) {
        self.completed_at = ts;
    }
    pub fn set_connection(&mut self, conn: Option<RunnerConnection>) {
        self.connection = conn;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum StageStatus {
    Pending,
    Running,
    WaitingForInput,
    Completed,
    Failed,
    TimedOut,
}

impl std::fmt::Display for StageStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StageStatus::Pending => write!(f, "pending"),
            StageStatus::Running => write!(f, "running"),
            StageStatus::WaitingForInput => write!(f, "waiting_for_input"),
            StageStatus::Completed => write!(f, "completed"),
            StageStatus::Failed => write!(f, "failed"),
            StageStatus::TimedOut => write!(f, "timed_out"),
        }
    }
}

impl FromStr for StageStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(StageStatus::Pending),
            "running" => Ok(StageStatus::Running),
            "waiting_for_input" | "WaitingForInput" => Ok(StageStatus::WaitingForInput),
            "completed" => Ok(StageStatus::Completed),
            "failed" => Ok(StageStatus::Failed),
            "timed_out" | "TimedOut" => Ok(StageStatus::TimedOut),
            _ => Err(format!("Unknown StageStatus: {}", s)),
        }
    }
}

pub struct Database {
    conn: Mutex<rusqlite::Connection>,
}

macro_rules! lock_conn {
    ($self:expr) => {
        match $self.conn.lock() {
            Ok(guard) => guard,
            Err(e) => {
                tracing::error!("DB mutex poisoned, recovering");
                e.into_inner()
            }
        }
    };
}

pub mod access;
mod budgets;
pub mod delivery;
mod effects;
mod lifecycle;
pub mod outputs;
pub mod packets;
pub mod plans;
pub mod process_lock;
mod provider;
pub mod publications;
#[cfg(test)]
mod question_tests;
mod questions;
mod recovery;
pub mod redaction;
pub use budgets::{
    BUDGET_INTERACTION_NUDGES, BUDGET_MODEL_CALLS, BUDGET_OUTPUT_CORRECTIONS,
    BUDGET_QUESTION_ROUNDS, BUDGET_TOOL_CALLS, BudgetCharge, BudgetSnapshot, RunBudgetLimits,
};
pub use effects::{EffectStatus, PendingEffect};
pub use provider::{ProviderAttempt, ProviderAttemptSummary, ProviderUsage, ProviderUsageSummary};
pub use questions::{
    MAX_PENDING_QUESTIONS_PER_RUN, QuestionAcceptance, QuestionRecord, QuestionStatus,
};
pub use recovery::{RecoveryState, RetryPlan};

impl Database {
    pub fn open(path: &str) -> Result<Self> {
        let expanded = shellexpand::env(path)
            .map_err(|e| anyhow::anyhow!("Failed to expand path '{}': {}", path, e))?
            .to_string();
        if let Some(parent) = PathBuf::from(&expanded).parent() {
            std::fs::create_dir_all(parent)?;
        }
        tracing::info!(path = %expanded, "Opening database");
        let conn = rusqlite::Connection::open(&expanded)
            .with_context(|| format!("Failed to open database: {}", expanded))?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")
            .with_context(|| "Failed to set WAL journal mode")?;
        let db = Self {
            conn: Mutex::new(conn),
        };
        db.migrate()?;
        tracing::info!("Database ready");
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        let conn = lock_conn!(self);
        tracing::debug!("Running database migrations");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS projects (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                repo_url TEXT,
                base_branch TEXT NOT NULL DEFAULT 'main',
                workspace_path TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS lanes (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL REFERENCES projects(id),
                name TEXT NOT NULL,
                smg_path TEXT NOT NULL,
                description TEXT,
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS pipeline_runs (
                id TEXT PRIMARY KEY,
                description TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'running',
                current_flow_id TEXT,
                gate_state TEXT,
                gate_instance_id TEXT,
                gate_name TEXT,
                repo_url TEXT,
                base_branch TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS flows (
                id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL REFERENCES pipeline_runs(id),
                flow_name TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                current_stage_id TEXT,
                created_at TEXT NOT NULL,
                started_at TEXT,
                completed_at TEXT
            );
            CREATE TABLE IF NOT EXISTS stages (
                id TEXT PRIMARY KEY,
                flow_id TEXT NOT NULL REFERENCES flows(id),
                stage_name TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                input_artifacts TEXT,
                output_artifacts TEXT,
                error TEXT,
                iteration INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                started_at TEXT,
                completed_at TEXT
            );",
        )?;

        let has_worktree_path: bool = conn
            .prepare("SELECT worktree_path FROM flows LIMIT 0")
            .map(|_| true)
            .unwrap_or(false);
        if !has_worktree_path {
            tracing::info!("Migrating: adding worktree_path/branch_name columns");
            conn.execute_batch(
                "ALTER TABLE flows ADD COLUMN worktree_path TEXT;
                 ALTER TABLE flows ADD COLUMN branch_name TEXT;",
            )?;
        }

        let has_repo_url: bool = conn
            .prepare("SELECT repo_url FROM pipeline_runs LIMIT 0")
            .map(|_| true)
            .unwrap_or(false);
        if !has_repo_url {
            tracing::info!("Migrating: adding repo_url/base_branch columns");
            conn.execute_batch(
                "ALTER TABLE pipeline_runs ADD COLUMN repo_url TEXT;
                 ALTER TABLE pipeline_runs ADD COLUMN base_branch TEXT;",
            )?;
        }

        let has_workspace_path: bool = conn
            .prepare("SELECT workspace_path FROM projects LIMIT 0")
            .map(|_| true)
            .unwrap_or(false);
        if !has_workspace_path {
            tracing::info!("Migrating: adding workspace_path column");
            conn.execute_batch("ALTER TABLE projects ADD COLUMN workspace_path TEXT;")?;
        }

        let has_project_id: bool = conn
            .prepare("SELECT project_id FROM pipeline_runs LIMIT 0")
            .map(|_| true)
            .unwrap_or(false);
        if !has_project_id {
            tracing::info!("Migrating: adding project_id/lane_id columns");
            conn.execute_batch(
                "ALTER TABLE pipeline_runs ADD COLUMN project_id TEXT;
                 ALTER TABLE pipeline_runs ADD COLUMN lane_id TEXT;",
            )?;
        }

        let has_runner_host: bool = conn
            .prepare("SELECT runner_host FROM stages LIMIT 0")
            .map(|_| true)
            .unwrap_or(false);
        if !has_runner_host {
            let has_container_host: bool = conn
                .prepare("SELECT container_host FROM stages LIMIT 0")
                .map(|_| true)
                .unwrap_or(false);
            if has_container_host {
                tracing::info!("Migrating: renaming container_* columns to runner_* in stages");
                conn.execute_batch(
                    "ALTER TABLE stages RENAME COLUMN container_host TO runner_host;
                     ALTER TABLE stages RENAME COLUMN container_port TO runner_port;
                     ALTER TABLE stages RENAME COLUMN container_password TO runner_password;
                     ALTER TABLE stages RENAME COLUMN container_session_id TO runner_session_id;",
                )?;
            } else {
                tracing::info!("Migrating: adding runner connection columns to stages");
                conn.execute_batch(
                    "ALTER TABLE stages ADD COLUMN runner_host TEXT;
                     ALTER TABLE stages ADD COLUMN runner_port INTEGER;
                     ALTER TABLE stages ADD COLUMN runner_password TEXT;
                     ALTER TABLE stages ADD COLUMN runner_session_id TEXT;",
                )?;
            }
        }

        let has_conversations: bool = conn
            .prepare("SELECT conversation_id FROM conversations LIMIT 0")
            .map(|_| true)
            .unwrap_or(false);
        if !has_conversations {
            tracing::info!("Migrating: creating conversations table");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS conversations (
                    id TEXT PRIMARY KEY,
                    run_id TEXT NOT NULL REFERENCES pipeline_runs(id),
                    conversation_type TEXT NOT NULL,
                    provider TEXT NOT NULL,
                    provider_key TEXT NOT NULL,
                    stage_name TEXT,
                    session_id TEXT,
                    created_at TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_conversations_run_id ON conversations(run_id);
                CREATE INDEX IF NOT EXISTS idx_conversations_provider_key ON conversations(provider_key);"
            )?;
        }

        let has_flow_number: bool = conn
            .prepare("SELECT flow_number FROM flows LIMIT 0")
            .map(|_| true)
            .unwrap_or(false);
        if !has_flow_number {
            tracing::info!("Migrating: adding flow_number column and counters table");
            conn.execute_batch(
                "ALTER TABLE flows ADD COLUMN flow_number INTEGER;
                 CREATE TABLE IF NOT EXISTS flow_counters (
                     project_id TEXT NOT NULL,
                     flow_name TEXT NOT NULL,
                     next_number INTEGER NOT NULL DEFAULT 1,
                     PRIMARY KEY (project_id, flow_name)
                 );",
            )?;
        }

        let has_trace_id: bool = conn
            .prepare("SELECT trace_id FROM pipeline_runs LIMIT 0")
            .map(|_| true)
            .unwrap_or(false);
        if !has_trace_id {
            tracing::info!("Migrating: adding trace_id column");
            conn.execute_batch("ALTER TABLE pipeline_runs ADD COLUMN trace_id TEXT;")?;
        }

        // Clean up JSON-quoted status values from older versions
        tracing::info!("Migrating: cleaning quoted status values");
        conn.execute_batch(
            "UPDATE stages SET status = REPLACE(status, '\"', '') WHERE status LIKE '\"%';
             UPDATE flows SET status = REPLACE(status, '\"', '') WHERE status LIKE '\"%';
             UPDATE pipeline_runs SET status = REPLACE(status, '\"', '') WHERE status LIKE '\"%';",
        )?;

        let has_slug: bool = conn
            .prepare("SELECT slug FROM pipeline_runs LIMIT 0")
            .map(|_| true)
            .unwrap_or(false);
        if !has_slug {
            tracing::info!("Migrating: adding slug, pr_url, pr_number columns");
            conn.execute_batch(
                "ALTER TABLE pipeline_runs ADD COLUMN slug TEXT;
                 ALTER TABLE pipeline_runs ADD COLUMN pr_url TEXT;
                 ALTER TABLE pipeline_runs ADD COLUMN pr_number INTEGER;
                 UPDATE pipeline_runs SET status = 'completed' WHERE status = 'waiting_for_merge';",
            )?;
        }

        // Migration: project_secrets table
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS project_secrets (
                project_id TEXT NOT NULL,
                key TEXT NOT NULL,
                encrypted_value TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (project_id, key)
            );",
        )
        .with_context(|| "Failed to create project_secrets table")?;

        // Migration: users, auth tokens, gate decisions
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS users (
                id TEXT PRIMARY KEY,
                username TEXT NOT NULL UNIQUE,
                password_hash TEXT NOT NULL,
                display_name TEXT,
                is_admin INTEGER NOT NULL DEFAULT 0,
                created_at TEXT,
                updated_at TEXT
            );
            CREATE TABLE IF NOT EXISTS auth_tokens (
                token TEXT PRIMARY KEY,
                user_id TEXT NOT NULL,
                created_at TEXT,
                last_used_at TEXT
            );",
        )
        .with_context(|| "Failed to create users/auth tables")?;

        // Migration: immutable versioned artifact store
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS artifacts (
                id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL,
                flow_name TEXT NOT NULL,
                stage_name TEXT NOT NULL,
                attempt INTEGER NOT NULL,
                version INTEGER NOT NULL,
                content TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_artifacts_run ON artifacts(run_id, flow_name, stage_name, version);"
        ).with_context(|| "Failed to create artifacts table")?;

        // Migration: enforce artifact version uniqueness (convention was race-prone)
        conn.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_artifacts_version ON artifacts(run_id, flow_name, stage_name, attempt, version);"
        ).with_context(|| "Failed to add artifacts version uniqueness")?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS gate_decisions (
                id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL,
                gate_name TEXT,
                actor_id TEXT,
                actor_name TEXT,
                approved INTEGER NOT NULL,
                reason TEXT,
                note TEXT,
                created_at TEXT,
                gate_instance_id TEXT,
                idempotency_key TEXT,
                expected_state_version INTEGER,
                state_version INTEGER,
                disposition TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_gate_decisions_run ON gate_decisions(run_id);",
        )
        .with_context(|| "Failed to create auth/gate tables")?;

        // Generic terminal gates bind to one immutable stage artifact instead
        // of a business packet/plan. Existing packet/plan tables stay untouched.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS run_gate_outputs (
                gate_instance_id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL,
                flow_name TEXT NOT NULL,
                stage_name TEXT NOT NULL,
                artifact_id TEXT NOT NULL,
                artifact_version INTEGER NOT NULL,
                content_hash TEXT NOT NULL,
                bound_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_run_gate_outputs_run ON run_gate_outputs(run_id);",
        )
        .with_context(|| "Failed to create generic gate output table")?;

        for (table, column, declaration) in [
            ("pipeline_runs", "gate_instance_id", "TEXT"),
            ("pipeline_runs", "gate_name", "TEXT"),
            ("gate_decisions", "gate_instance_id", "TEXT"),
            ("gate_decisions", "idempotency_key", "TEXT"),
            ("gate_decisions", "expected_state_version", "INTEGER"),
            ("gate_decisions", "state_version", "INTEGER"),
            ("gate_decisions", "disposition", "TEXT"),
            ("gate_decisions", "output_artifact_id", "TEXT"),
            ("gate_decisions", "output_content_hash", "TEXT"),
            ("gate_decisions", "output_stage_name", "TEXT"),
            (
                "gate_decisions",
                "acknowledge_incomplete",
                "INTEGER NOT NULL DEFAULT 0",
            ),
        ] {
            if conn
                .prepare(&format!("SELECT {column} FROM {table} LIMIT 0"))
                .is_err()
            {
                conn.execute_batch(&format!(
                    "ALTER TABLE {table} ADD COLUMN {column} {declaration};"
                ))
                .with_context(|| format!("Failed to migrate {table}.{column}"))?;
            }
        }
        conn.execute_batch(
            "UPDATE pipeline_runs SET gate_instance_id = 'legacy_gate_' || id,
                 gate_name = COALESCE(gate_name, 'gate')
             WHERE status = 'waiting_for_gate' AND gate_instance_id IS NULL;
             CREATE UNIQUE INDEX IF NOT EXISTS idx_pipeline_runs_gate_instance
                 ON pipeline_runs(gate_instance_id) WHERE gate_instance_id IS NOT NULL;
             CREATE UNIQUE INDEX IF NOT EXISTS idx_gate_decisions_gate_instance
                 ON gate_decisions(gate_instance_id) WHERE gate_instance_id IS NOT NULL;
             CREATE UNIQUE INDEX IF NOT EXISTS idx_gate_decisions_idempotency
                 ON gate_decisions(idempotency_key) WHERE idempotency_key IS NOT NULL;",
        )
        .with_context(|| "Failed to migrate durable gate decision constraints")?;

        // Migration: append-only run event log
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS events (
                id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL,
                kind TEXT NOT NULL,
                actor TEXT,
                flow_name TEXT,
                stage_name TEXT,
                summary TEXT,
                payload TEXT,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_events_run ON events(run_id, created_at);

            CREATE TABLE IF NOT EXISTS stage_logs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id TEXT NOT NULL,
                stage_id TEXT NOT NULL,
                stage_name TEXT,
                kind TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_stage_logs_run ON stage_logs(run_id, id);",
        )
        .with_context(|| "Failed to create events table")?;

        // Migration: flow_name column on stage_logs
        let has_flow_name: bool = conn
            .prepare("SELECT flow_name FROM stage_logs LIMIT 0")
            .map(|_| true)
            .unwrap_or(false);
        if !has_flow_name {
            tracing::info!("Migrating: adding flow_name column to stage_logs");
            conn.execute_batch("ALTER TABLE stage_logs ADD COLUMN flow_name TEXT;")?;
        }

        // Workflow registry: immutable, content-addressed SMG versions.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS workflow_versions (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                smg_path TEXT NOT NULL,
                version INTEGER NOT NULL DEFAULT 0,
                revision INTEGER NOT NULL DEFAULT 1,
                description TEXT,
                content_hash TEXT NOT NULL UNIQUE,
                content_json TEXT NOT NULL,
                entry TEXT,
                exit TEXT,
                flow_count INTEGER NOT NULL DEFAULT 0,
                gate_count INTEGER NOT NULL DEFAULT 0,
                stage_count INTEGER NOT NULL DEFAULT 0,
                catalog_id TEXT,
                catalog_label TEXT,
                catalog_description TEXT,
                catalog_default INTEGER NOT NULL DEFAULT 0,
                capability_schema_version INTEGER NOT NULL DEFAULT 0,
                capability_hash TEXT,
                source_id TEXT,
                source_rel_path TEXT,
                source_kind TEXT,
                created_at TEXT NOT NULL
            );",
        )?;

        // Additive registry metadata for databases created before catalogs.
        for (column, declaration) in [
            ("catalog_id", "TEXT"),
            ("catalog_label", "TEXT"),
            ("catalog_description", "TEXT"),
            ("catalog_default", "INTEGER NOT NULL DEFAULT 0"),
            ("capability_schema_version", "INTEGER NOT NULL DEFAULT 0"),
            ("capability_hash", "TEXT"),
            ("source_id", "TEXT"),
            ("source_rel_path", "TEXT"),
            ("source_kind", "TEXT"),
        ] {
            if conn
                .prepare(&format!("SELECT {column} FROM workflow_versions LIMIT 0"))
                .is_err()
            {
                tracing::info!("Migrating: adding workflow_versions.{column}");
                conn.execute_batch(&format!(
                    "ALTER TABLE workflow_versions ADD COLUMN {column} {declaration};"
                ))?;
            }
        }
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_workflow_catalog
                 ON workflow_versions(catalog_id, revision DESC);
             CREATE TABLE IF NOT EXISTS workflow_assets (
                 workflow_version_id TEXT NOT NULL,
                 logical_path TEXT NOT NULL,
                 content_hash TEXT NOT NULL,
                 content BLOB NOT NULL,
                 PRIMARY KEY(workflow_version_id, logical_path)
             );",
        )
        .with_context(|| "Failed to create immutable workflow_assets table")?;

        // Lanes pin a workflow version; runs record which one they executed.
        for table in ["lanes", "pipeline_runs"] {
            let has_col: bool = conn
                .prepare(&format!("SELECT workflow_id FROM {} LIMIT 0", table))
                .map(|_| true)
                .unwrap_or(false);
            if !has_col {
                tracing::info!("Migrating: adding workflow_id column to {}", table);
                conn.execute_batch(&format!(
                    "ALTER TABLE {} ADD COLUMN workflow_id TEXT;",
                    table
                ))?;
            }
        }

        // TJ-012: immutable execution bundle identity on the run. Rows admitted
        // before this migration have NULL columns and are explicitly legacy.
        for (column, declaration) in [
            ("execution_bundle_id", "TEXT"),
            ("execution_bundle_hash", "TEXT"),
        ] {
            if conn
                .prepare(&format!("SELECT {column} FROM pipeline_runs LIMIT 0"))
                .is_err()
            {
                tracing::info!("Migrating: adding pipeline_runs.{column}");
                conn.execute_batch(&format!(
                    "ALTER TABLE pipeline_runs ADD COLUMN {column} {declaration};"
                ))?;
            }
        }
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS execution_bundles (
                id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL UNIQUE,
                workflow_id TEXT,
                bundle_hash TEXT NOT NULL,
                bundle_json TEXT NOT NULL,
                legacy INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_execution_bundles_run ON execution_bundles(run_id);",
        )
        .with_context(|| "Failed to create execution_bundles table")?;

        // TJ-014: durable question identity and exactly-one answer acceptance.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS questions (
                id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL,
                flow_id TEXT NOT NULL,
                stage_id TEXT NOT NULL,
                stage_name TEXT NOT NULL,
                flow_name TEXT NOT NULL,
                attempt INTEGER NOT NULL,
                request TEXT NOT NULL,
                status TEXT NOT NULL,
                answer TEXT,
                answer_source TEXT,
                answer_idempotency_key TEXT,
                created_at TEXT NOT NULL,
                answered_at TEXT,
                closed_at TEXT,
                close_reason TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_questions_run ON questions(run_id, status);
            CREATE INDEX IF NOT EXISTS idx_questions_stage ON questions(stage_id, status);
            CREATE UNIQUE INDEX IF NOT EXISTS idx_questions_idempotency
                ON questions(answer_idempotency_key) WHERE answer_idempotency_key IS NOT NULL;",
        )
        .with_context(|| "Failed to create questions table")?;

        if conn
            .prepare("SELECT profile FROM projects LIMIT 0")
            .is_err()
        {
            conn.execute_batch(
                "ALTER TABLE projects ADD COLUMN profile TEXT NOT NULL DEFAULT 'full';",
            )?;
        }
        for column in [
            "provisioning_state",
            "provisioning_error",
            "workspace_kind",
            "catalog_id",
        ] {
            if conn
                .prepare(&format!("SELECT {column} FROM projects LIMIT 0"))
                .is_err()
            {
                conn.execute_batch(&format!("ALTER TABLE projects ADD COLUMN {column} TEXT;"))?;
            }
        }

        // Every project must have at least one workflow (lane) — backfill a
        // Default lane for projects created before lanes were mandatory.
        conn.execute_batch(
            "INSERT INTO lanes (id, project_id, name, smg_path, created_at)
             SELECT 'lane_default_' || p.id, p.id, 'Default', '', strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
             FROM projects p WHERE NOT EXISTS (SELECT 1 FROM lanes l WHERE l.project_id = p.id);",
        )?;

        // Migration: git_provider column on projects
        let has_git_provider: bool = conn
            .prepare("SELECT git_provider FROM projects LIMIT 0")
            .map(|_| true)
            .unwrap_or(false);
        if !has_git_provider {
            tracing::info!("Migrating: adding git_provider column to projects");
            conn.execute_batch("ALTER TABLE projects ADD COLUMN git_provider TEXT;")?;
        }

        lifecycle::migrate(&conn)?;
        access::migrate(&conn)?;
        budgets::migrate(&conn)?;
        effects::migrate(&conn)?;
        recovery::migrate(&conn)?;
        packets::migrate(&conn)?;
        plans::migrate(&conn)?;
        publications::migrate(&conn)?;
        delivery::migrate(&conn)?;
        provider::migrate(&conn)?;
        conn.execute_batch("PRAGMA user_version = 1;")?;
        Ok(())
    }

    /// Schema version exposed to readiness/backup tooling. `PRAGMA
    /// user_version` is updated only after every migration in `migrate()`
    /// succeeds, so an interrupted upgrade never reports the new version.
    pub fn schema_version(&self) -> Result<i64> {
        let conn = lock_conn!(self);
        Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
    }

    pub fn integrity_check(&self) -> Result<()> {
        let conn = lock_conn!(self);
        let result: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        anyhow::ensure!(result == "ok", "SQLite integrity check failed: {result}");
        Ok(())
    }

    /// Consistent online backup of the live database using SQLite's VACUUM
    /// INTO. The destination must not already exist; callers restore into a
    /// fresh directory or explicitly remove a previous attempt.
    pub fn backup_to(&self, destination: &Path) -> Result<()> {
        if destination.exists() {
            anyhow::bail!(
                "backup destination already exists: {}",
                destination.display()
            );
        }
        let conn = lock_conn!(self);
        conn.execute("VACUUM INTO ?1", params![destination.to_string_lossy()])?;
        Ok(())
    }

    /// Ciphertext-only secret inventory for backup verification. The master
    /// key is always external and is never stored in the database/backup.
    pub fn secret_ciphertexts(&self) -> Result<Vec<(String, String, String)>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(
            "SELECT project_id, key, encrypted_value FROM project_secrets ORDER BY project_id, key",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn save_run(&self, run: &PipelineRun) -> Result<()> {
        let start = std::time::Instant::now();
        let mut conn = lock_conn!(self);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        lifecycle::guard_write(
            &tx,
            "pipeline_runs",
            &run.id,
            &run.revision,
            &run.status.to_string(),
        )?;
        let changed_deadline: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM pipeline_runs WHERE id = ?1 AND deadline_at IS NOT ?2)",
            params![run.id, run.deadline_at],
            |row| row.get(0),
        )?;
        anyhow::ensure!(
            !changed_deadline,
            "Run deadline is immutable after creation"
        );
        if let Some(deadline) = &run.deadline_at {
            chrono::DateTime::parse_from_rfc3339(deadline).context("Invalid run deadline")?;
        }
        tracing::debug!(run_id = %run.id, status = ?run.status, "Saving run");
        tx.execute(
            "INSERT INTO pipeline_runs (id, project_id, lane_id, workflow_id, execution_bundle_id, execution_bundle_hash, description, status, current_flow_id, gate_state, repo_url, base_branch, trace_id, slug, pr_url, pr_number, created_at, updated_at, deadline_at, gate_instance_id, gate_name)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
             ON CONFLICT(id) DO UPDATE SET project_id=excluded.project_id, lane_id=excluded.lane_id,
             workflow_id=excluded.workflow_id,
             execution_bundle_id=COALESCE(pipeline_runs.execution_bundle_id, excluded.execution_bundle_id),
             execution_bundle_hash=COALESCE(pipeline_runs.execution_bundle_hash, excluded.execution_bundle_hash),
             description=excluded.description, status=excluded.status,
             current_flow_id=excluded.current_flow_id,
             gate_state=CASE WHEN excluded.status='waiting_for_gate' THEN excluded.gate_state ELSE NULL END,
             repo_url=excluded.repo_url,
             base_branch=excluded.base_branch, trace_id=excluded.trace_id, slug=excluded.slug,
             pr_url=excluded.pr_url, pr_number=excluded.pr_number, updated_at=excluded.updated_at,
             gate_instance_id=CASE WHEN excluded.status='waiting_for_gate' THEN excluded.gate_instance_id ELSE NULL END,
             gate_name=CASE WHEN excluded.status='waiting_for_gate' THEN excluded.gate_name ELSE NULL END",
            params![run.id, run.project_id.as_ref().map(|p| p.as_str()), run.lane_id.as_ref().map(|l| l.as_str()), run.workflow_id, run.execution_bundle_id, run.execution_bundle_hash, run.description, run.status.to_string(), run.current_flow_id, run.gate_state, run.repo_url, run.base_branch, run.trace_id, run.slug, run.pr_url, run.pr_number, run.created_at, run.updated_at, run.deadline_at, run.gate_instance_id, run.gate_name],
        ).with_context(|| format!("Failed to save run {}", run.id))?;
        lifecycle::finish_write(
            &tx,
            "pipeline_runs",
            &run.id,
            &run.id,
            &run.status.to_string(),
            &run.revision,
        )?;
        if matches!(
            run.status,
            RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled
        ) {
            questions::close_open_for_run_tx(&tx, &run.id, "run reached a terminal state")?;
        }
        tx.commit()?;
        run.revision.advance();
        let elapsed = start.elapsed();
        if elapsed.as_secs() >= 1 {
            tracing::warn!(
                elapsed_ms = elapsed.as_millis() as u64,
                operation = "save_run",
                "Slow DB operation"
            );
        } else {
            tracing::debug!(
                elapsed_ms = elapsed.as_millis() as u64,
                operation = "save_run",
                "DB operation completed"
            );
        }
        Ok(())
    }

    fn read_run_row(row: &rusqlite::Row) -> rusqlite::Result<PipelineRun> {
        Ok(PipelineRun {
            revision: lifecycle::Revision::loaded(row.get(18)?),
            deadline_at: row.get(19)?,
            id: row.get(0)?,
            project_id: row
                .get::<_, Option<String>>(1)
                .unwrap_or(None)
                .map(ProjectId),
            lane_id: row.get::<_, Option<String>>(2).unwrap_or(None).map(LaneId),
            workflow_id: row.get::<_, Option<String>>(3).unwrap_or(None),
            execution_bundle_id: row.get::<_, Option<String>>(4).unwrap_or(None),
            execution_bundle_hash: row.get::<_, Option<String>>(5).unwrap_or(None),
            description: row.get(6)?,
            status: row
                .get::<_, String>(7)?
                .parse()
                .unwrap_or(RunStatus::Running),
            current_flow_id: row.get(8)?,
            gate_state: row.get(9)?,
            repo_url: row.get(10)?,
            base_branch: row.get(11)?,
            trace_id: row.get(12).unwrap_or(None),
            slug: row.get(13).unwrap_or(None),
            pr_url: row.get(14).unwrap_or(None),
            pr_number: row.get(15).unwrap_or(None),
            created_at: row.get(16)?,
            updated_at: row.get(17)?,
            gate_instance_id: row.get(20)?,
            gate_name: row.get(21)?,
        })
    }

    pub fn load_run(&self, id: &str) -> Result<Option<PipelineRun>> {
        let start = std::time::Instant::now();
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare("SELECT id, project_id, lane_id, workflow_id, execution_bundle_id, execution_bundle_hash, description, status, current_flow_id, gate_state, repo_url, base_branch, trace_id, slug, pr_url, pr_number, created_at, updated_at, revision, deadline_at, gate_instance_id, gate_name FROM pipeline_runs WHERE id = ?1")?;
        let mut rows = stmt.query(params![id])?;
        let result = match rows.next()? {
            Some(row) => Ok(Some(Self::read_run_row(row)?)),
            None => Ok(None),
        };
        let elapsed = start.elapsed();
        if elapsed.as_secs() >= 1 {
            tracing::warn!(
                elapsed_ms = elapsed.as_millis() as u64,
                operation = "load_run",
                "Slow DB operation"
            );
        } else {
            tracing::debug!(
                elapsed_ms = elapsed.as_millis() as u64,
                operation = "load_run",
                "DB operation completed"
            );
        }
        result
    }

    pub fn list_runs(&self) -> Result<Vec<PipelineRun>> {
        let start = std::time::Instant::now();
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare("SELECT id, project_id, lane_id, workflow_id, execution_bundle_id, execution_bundle_hash, description, status, current_flow_id, gate_state, repo_url, base_branch, trace_id, slug, pr_url, pr_number, created_at, updated_at, revision, deadline_at, gate_instance_id, gate_name FROM pipeline_runs ORDER BY rowid DESC")?;
        let rows = stmt.query_map([], Self::read_run_row)?;
        let mut runs = Vec::new();
        for row in rows {
            runs.push(row?);
        }
        let elapsed = start.elapsed();
        if elapsed.as_secs() >= 1 {
            tracing::warn!(
                elapsed_ms = elapsed.as_millis() as u64,
                operation = "list_runs",
                "Slow DB operation"
            );
        } else {
            tracing::debug!(
                elapsed_ms = elapsed.as_millis() as u64,
                operation = "list_runs",
                "DB operation completed"
            );
        }
        Ok(runs)
    }

    pub fn save_flow(&self, flow: &FlowRecord) -> Result<()> {
        let start = std::time::Instant::now();
        let mut conn = lock_conn!(self);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        lifecycle::guard_write(
            &tx,
            "flows",
            &flow.id,
            &flow.revision,
            &flow.status.to_string(),
        )?;
        lifecycle::guard_flow_parent(&tx, flow.run_id.as_str(), &flow.id)?;
        tracing::debug!(flow_id = %flow.id, run_id = %flow.run_id, flow = %flow.flow_name, number = ?flow.flow_number, status = ?flow.status, "Saving flow");
        tx.execute(
            "INSERT INTO flows (id, run_id, flow_name, flow_number, status, current_stage_id, worktree_path, branch_name, created_at, started_at, completed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(id) DO UPDATE SET status=excluded.status, current_stage_id=excluded.current_stage_id,
             worktree_path=excluded.worktree_path, branch_name=excluded.branch_name,
             started_at=excluded.started_at, completed_at=excluded.completed_at",
            params![flow.id, flow.run_id.as_str(), flow.flow_name, flow.flow_number, flow.status.to_string(), flow.current_stage_id, flow.worktree_path, flow.branch_name, flow.created_at, flow.started_at, flow.completed_at],
        ).with_context(|| format!("Failed to save flow {}", flow.id))?;
        lifecycle::finish_write(
            &tx,
            "flows",
            &flow.id,
            flow.run_id.as_str(),
            &flow.status.to_string(),
            &flow.revision,
        )?;
        tx.commit()?;
        flow.revision.advance();
        let elapsed = start.elapsed();
        if elapsed.as_secs() >= 1 {
            tracing::warn!(
                elapsed_ms = elapsed.as_millis() as u64,
                operation = "save_flow",
                "Slow DB operation"
            );
        } else {
            tracing::debug!(
                elapsed_ms = elapsed.as_millis() as u64,
                operation = "save_flow",
                "DB operation completed"
            );
        }
        Ok(())
    }

    pub fn load_flow(&self, id: &str) -> Result<Option<FlowRecord>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare("SELECT id, run_id, flow_name, flow_number, status, current_stage_id, worktree_path, branch_name, created_at, started_at, completed_at, revision FROM flows WHERE id = ?1")?;
        let mut rows = stmt.query(params![id])?;
        match rows.next()? {
            Some(row) => Ok(Some(Self::read_flow_row(row)?)),
            None => Ok(None),
        }
    }

    pub fn load_flows_for_run(&self, run_id: &str) -> Result<Vec<FlowRecord>> {
        let start = std::time::Instant::now();
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare("SELECT id, run_id, flow_name, flow_number, status, current_stage_id, worktree_path, branch_name, created_at, started_at, completed_at, revision FROM flows WHERE run_id = ?1 ORDER BY rowid")?;
        let rows = stmt.query_map(params![run_id], Self::read_flow_row)?;
        let mut flows = Vec::new();
        for row in rows {
            flows.push(row?);
        }
        let elapsed = start.elapsed();
        if elapsed.as_secs() >= 1 {
            tracing::warn!(
                elapsed_ms = elapsed.as_millis() as u64,
                operation = "load_flows_for_run",
                "Slow DB operation"
            );
        } else {
            tracing::debug!(
                elapsed_ms = elapsed.as_millis() as u64,
                operation = "load_flows_for_run",
                "DB operation completed"
            );
        }
        Ok(flows)
    }

    pub fn save_stage(&self, stage: &StageRecord) -> Result<()> {
        let start = std::time::Instant::now();
        let mut conn = lock_conn!(self);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        lifecycle::guard_write(
            &tx,
            "stages",
            &stage.id,
            &stage.revision,
            &stage.status.to_string(),
        )?;
        lifecycle::guard_stage_parent(&tx, stage.flow_id.as_str())?;
        tracing::debug!(stage_id = %stage.id, stage = %stage.stage_name, status = ?stage.status, "Saving stage");
        tx.execute(
            "INSERT INTO stages (id, flow_id, stage_name, status, input_artifacts, output_artifacts, error, iteration, created_at, started_at, completed_at, runner_host, runner_port, runner_password, runner_session_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
             ON CONFLICT(id) DO UPDATE SET status=excluded.status, input_artifacts=excluded.input_artifacts,
             output_artifacts=excluded.output_artifacts, error=excluded.error, started_at=excluded.started_at,
             completed_at=excluded.completed_at, runner_host=excluded.runner_host, runner_port=excluded.runner_port,
             runner_password=excluded.runner_password, runner_session_id=excluded.runner_session_id",
            params![stage.id, stage.flow_id.as_str(), stage.stage_name, stage.status.to_string(), stage.input_artifacts, stage.output_artifacts, stage.error, stage.iteration, stage.created_at, stage.started_at, stage.completed_at, stage.connection.as_ref().and_then(|c| c.host.as_ref()), stage.connection.as_ref().and_then(|c| c.port), stage.connection.as_ref().and_then(|c| c.password.as_ref()), stage.connection.as_ref().and_then(|c| c.session_id.as_ref())],
        ).with_context(|| format!("Failed to save stage {}", stage.id))?;
        let run_id: String = tx.query_row(
            "SELECT run_id FROM flows WHERE id = ?1",
            params![stage.flow_id.as_str()],
            |row| row.get(0),
        )?;
        lifecycle::finish_write(
            &tx,
            "stages",
            &stage.id,
            &run_id,
            &stage.status.to_string(),
            &stage.revision,
        )?;
        if matches!(
            stage.status,
            StageStatus::Completed | StageStatus::Failed | StageStatus::TimedOut
        ) {
            tx.execute(
                "UPDATE questions SET status = 'cancelled', closed_at = ?1,
                     close_reason = 'stage reached a terminal state'
                 WHERE stage_id = ?2 AND status = 'open'",
                params![chrono::Utc::now().to_rfc3339(), stage.id],
            )?;
        }
        tx.commit()?;
        stage.revision.advance();
        let elapsed = start.elapsed();
        if elapsed.as_secs() >= 1 {
            tracing::warn!(
                elapsed_ms = elapsed.as_millis() as u64,
                operation = "save_stage",
                "Slow DB operation"
            );
        } else {
            tracing::debug!(
                elapsed_ms = elapsed.as_millis() as u64,
                operation = "save_stage",
                "DB operation completed"
            );
        }
        Ok(())
    }

    fn read_flow_row(row: &rusqlite::Row) -> rusqlite::Result<FlowRecord> {
        Ok(FlowRecord {
            revision: lifecycle::Revision::loaded(row.get(11)?),
            id: row.get(0)?,
            run_id: RunId(row.get(1)?),
            flow_name: row.get(2)?,
            flow_number: row.get(3)?,
            status: row
                .get::<_, String>(4)?
                .parse()
                .unwrap_or(FlowStatus::Pending),
            current_stage_id: row.get(5)?,
            worktree_path: row.get(6)?,
            branch_name: row.get(7)?,
            created_at: row.get(8)?,
            started_at: row.get(9)?,
            completed_at: row.get(10)?,
        })
    }

    fn read_stage_row(row: &rusqlite::Row) -> rusqlite::Result<StageRecord> {
        let runner_host: Option<String> = row.get(11).unwrap_or(None);
        let runner_port: Option<u16> = row.get(12).unwrap_or(None);
        let runner_password: Option<String> = row.get(13).unwrap_or(None);
        let runner_session_id: Option<String> = row.get(14).unwrap_or(None);

        let connection = if runner_host.is_some()
            || runner_port.is_some()
            || runner_password.is_some()
            || runner_session_id.is_some()
        {
            Some(RunnerConnection {
                host: runner_host,
                port: runner_port,
                password: runner_password,
                session_id: runner_session_id,
            })
        } else {
            None
        };

        Ok(StageRecord {
            revision: lifecycle::Revision::loaded(row.get(15)?),
            id: row.get(0)?,
            flow_id: FlowId(row.get(1)?),
            stage_name: row.get(2)?,
            status: row
                .get::<_, String>(3)?
                .parse()
                .unwrap_or(StageStatus::Pending),
            input_artifacts: row.get(4)?,
            output_artifacts: row.get(5)?,
            error: row.get(6)?,
            iteration: row.get(7)?,
            created_at: row.get(8)?,
            started_at: row.get(9)?,
            completed_at: row.get(10)?,
            connection,
        })
    }

    pub fn load_stage(&self, id: &str) -> Result<Option<StageRecord>> {
        let start = std::time::Instant::now();
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare("SELECT id, flow_id, stage_name, status, input_artifacts, output_artifacts, error, iteration, created_at, started_at, completed_at, runner_host, runner_port, runner_password, runner_session_id, revision FROM stages WHERE id = ?1")?;
        let mut rows = stmt.query(params![id])?;
        let result = match rows.next()? {
            Some(row) => Ok(Some(Self::read_stage_row(row)?)),
            None => Ok(None),
        };
        let elapsed = start.elapsed();
        if elapsed.as_secs() >= 1 {
            tracing::warn!(
                elapsed_ms = elapsed.as_millis() as u64,
                operation = "load_stage",
                "Slow DB operation"
            );
        } else {
            tracing::debug!(
                elapsed_ms = elapsed.as_millis() as u64,
                operation = "load_stage",
                "DB operation completed"
            );
        }
        result
    }

    pub fn load_stages_for_flow(&self, flow_id: &str) -> Result<Vec<StageRecord>> {
        let start = std::time::Instant::now();
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare("SELECT id, flow_id, stage_name, status, input_artifacts, output_artifacts, error, iteration, created_at, started_at, completed_at, runner_host, runner_port, runner_password, runner_session_id, revision FROM stages WHERE flow_id = ?1 ORDER BY rowid")?;
        let rows = stmt.query_map(params![flow_id], Self::read_stage_row)?;
        let mut stages = Vec::new();
        for row in rows {
            stages.push(row?);
        }
        let elapsed = start.elapsed();
        if elapsed.as_secs() >= 1 {
            tracing::warn!(
                elapsed_ms = elapsed.as_millis() as u64,
                operation = "load_stages_for_flow",
                "Slow DB operation"
            );
        } else {
            tracing::debug!(
                elapsed_ms = elapsed.as_millis() as u64,
                operation = "load_stages_for_flow",
                "DB operation completed"
            );
        }
        Ok(stages)
    }

    pub fn load_stage_by_name(
        &self,
        flow_id: &str,
        stage_name: &str,
    ) -> Result<Option<StageRecord>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare("SELECT id, flow_id, stage_name, status, input_artifacts, output_artifacts, error, iteration, created_at, started_at, completed_at, runner_host, runner_port, runner_password, runner_session_id, revision FROM stages WHERE flow_id = ?1 AND stage_name = ?2 ORDER BY rowid DESC LIMIT 1")?;
        let mut rows = stmt.query_map(params![flow_id, stage_name], Self::read_stage_row)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    pub fn find_stale_stages(&self, older_than_secs: u64) -> Result<Vec<StageRecord>> {
        let conn = lock_conn!(self);
        let cutoff = chrono::Utc::now() - chrono::Duration::seconds(older_than_secs as i64);
        let cutoff_str = cutoff.to_rfc3339();
        let mut stmt = conn.prepare(
            "SELECT id, flow_id, stage_name, status, input_artifacts, output_artifacts, error, iteration, created_at, started_at, completed_at, runner_host, runner_port, runner_password, runner_session_id, revision
             FROM stages WHERE status = 'running' AND created_at < ?1"
        )?;
        let rows = stmt.query_map(params![cutoff_str], Self::read_stage_row)?;
        let mut stages = Vec::new();
        for row in rows {
            stages.push(row?);
        }
        Ok(stages)
    }

    pub fn save_project(&self, project: &Project) -> Result<()> {
        let conn = lock_conn!(self);
        conn.execute(
            "INSERT OR REPLACE INTO projects (id, name, repo_url, base_branch, workspace_path, git_provider, profile, workspace_kind, catalog_id, provisioning_state, provisioning_error, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![project.id, project.name, project.repo_url, project.base_branch, project.workspace_path, project.git_provider, project.profile, project.workspace_kind, project.catalog_id, project.provisioning_state, project.provisioning_error, project.created_at, project.updated_at],
        ).with_context(|| format!("Failed to save project {}", project.id))?;
        Ok(())
    }

    fn read_project_row(row: &rusqlite::Row) -> rusqlite::Result<Project> {
        Ok(Project {
            id: row.get(0)?,
            name: row.get(1)?,
            repo_url: row.get(2)?,
            base_branch: row.get(3)?,
            workspace_path: row.get(4)?,
            git_provider: row.get(5).unwrap_or(None),
            profile: row.get(6).unwrap_or_else(|_| default_project_profile()),
            workspace_kind: row.get(7).unwrap_or(None),
            catalog_id: row.get(8).unwrap_or(None),
            provisioning_state: row.get(9).unwrap_or(None),
            provisioning_error: row.get(10).unwrap_or(None),
            created_at: row.get(11)?,
            updated_at: row.get(12)?,
        })
    }

    pub fn load_project(&self, id: &str) -> Result<Option<Project>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare("SELECT id, name, repo_url, base_branch, workspace_path, git_provider, profile, workspace_kind, catalog_id, provisioning_state, provisioning_error, created_at, updated_at FROM projects WHERE id = ?1")?;
        let mut rows = stmt.query(params![id])?;
        match rows.next()? {
            Some(row) => Ok(Some(Self::read_project_row(row)?)),
            None => Ok(None),
        }
    }

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare("SELECT id, name, repo_url, base_branch, workspace_path, git_provider, profile, workspace_kind, catalog_id, provisioning_state, provisioning_error, created_at, updated_at FROM projects ORDER BY rowid DESC")?;
        let rows = stmt.query_map([], Self::read_project_row)?;
        let mut projects = Vec::new();
        for row in rows {
            projects.push(row?);
        }
        Ok(projects)
    }

    pub fn delete_project(&self, id: &str) -> Result<()> {
        let conn = lock_conn!(self);
        conn.execute("DELETE FROM projects WHERE id = ?1", params![id])
            .with_context(|| format!("Failed to delete project {}", id))?;
        Ok(())
    }

    fn read_lane_row(row: &rusqlite::Row) -> rusqlite::Result<Lane> {
        Ok(Lane {
            id: row.get(0)?,
            project_id: ProjectId(row.get(1)?),
            name: row.get(2)?,
            smg_path: row.get(3)?,
            description: row.get(4)?,
            workflow_id: row.get(5).unwrap_or(None),
            created_at: row.get(6)?,
        })
    }

    pub fn save_lane(&self, lane: &Lane) -> Result<()> {
        let conn = lock_conn!(self);
        conn.execute(
            "INSERT OR REPLACE INTO lanes (id, project_id, name, smg_path, description, workflow_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![lane.id, lane.project_id.as_str(), lane.name, lane.smg_path, lane.description, lane.workflow_id, lane.created_at],
        ).with_context(|| format!("Failed to save lane {}", lane.id))?;
        Ok(())
    }

    pub fn load_lane(&self, id: &str) -> Result<Option<Lane>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare("SELECT id, project_id, name, smg_path, description, workflow_id, created_at FROM lanes WHERE id = ?1")?;
        let mut rows = stmt.query(params![id])?;
        match rows.next()? {
            Some(row) => Ok(Some(Self::read_lane_row(row)?)),
            None => Ok(None),
        }
    }

    pub fn list_lanes(&self) -> Result<Vec<Lane>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare("SELECT id, project_id, name, smg_path, description, workflow_id, created_at FROM lanes ORDER BY rowid DESC")?;
        let rows = stmt.query_map([], Self::read_lane_row)?;
        let mut lanes = Vec::new();
        for row in rows {
            lanes.push(row?);
        }
        Ok(lanes)
    }

    pub fn list_lanes_for_project(&self, project_id: &str) -> Result<Vec<Lane>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare("SELECT id, project_id, name, smg_path, description, workflow_id, created_at FROM lanes WHERE project_id = ?1 ORDER BY rowid")?;
        let rows = stmt.query_map(params![project_id], Self::read_lane_row)?;
        let mut lanes = Vec::new();
        for row in rows {
            lanes.push(row?);
        }
        Ok(lanes)
    }

    // ---- Workflow registry ----

    /// Registers SMG content as an immutable version. Content-addressed:
    /// identical content returns the existing version unchanged. New content
    /// gets the next revision for its workflow name.
    pub fn register_workflow(&self, wf: &WorkflowVersion) -> Result<WorkflowVersion> {
        self.register_workflow_inner(wf, &[])
    }

    /// Register a package workflow version and all of its immutable assets in
    /// one transaction. A graph whose content hash is already registered with
    /// different assets fails instead of silently keeping stale prompts or
    /// schemas; identical re-imports remain idempotent.
    pub fn register_workflow_with_assets(
        &self,
        wf: &WorkflowVersion,
        assets: &[WorkflowAsset],
    ) -> Result<WorkflowVersion> {
        let mut seen = std::collections::HashSet::new();
        for asset in assets {
            anyhow::ensure!(
                seen.insert(asset.logical_path.as_str()),
                "duplicate package asset logical path '{}'; nothing was persisted",
                asset.logical_path
            );
            let actual = hex::encode(Sha256::digest(&asset.content));
            anyhow::ensure!(
                actual == asset.content_hash,
                "package asset '{}' content hash mismatch; nothing was persisted",
                asset.logical_path
            );
        }
        self.register_workflow_inner(wf, assets)
    }

    fn register_workflow_inner(
        &self,
        wf: &WorkflowVersion,
        assets: &[WorkflowAsset],
    ) -> Result<WorkflowVersion> {
        // Hash check, revision assignment and insert happen in one immediate
        // transaction so concurrent imports cannot duplicate revisions.
        let conn = lock_conn!(self);
        let tx = conn.unchecked_transaction()?;
        let existing: Option<WorkflowVersion> = {
            let mut stmt = tx.prepare("SELECT id, name, smg_path, version, revision, description, content_hash, content_json, entry, exit, flow_count, gate_count, stage_count, catalog_id, catalog_label, catalog_description, catalog_default, capability_schema_version, capability_hash, source_id, source_rel_path, source_kind, created_at FROM workflow_versions WHERE content_hash = ?1")?;
            let mut rows = stmt.query(params![wf.content_hash])?;
            match rows.next()? {
                Some(row) => Some(Self::read_workflow_row(row)?),
                None => None,
            }
        };
        if let Some(existing) = existing {
            if !assets.is_empty() {
                let stored = Self::read_workflow_assets(&tx, &existing.id)?;
                let same = stored.len() == assets.len()
                    && stored.iter().all(|stored| {
                        assets.iter().any(|asset| {
                            asset.logical_path == stored.logical_path
                                && asset.content_hash == stored.content_hash
                                && asset.content == stored.content
                        })
                    });
                anyhow::ensure!(
                    same,
                    "workflow '{}' content hash {} is already registered with different package assets; update workflow.smg.json (or its version/name) so the graph content changes, then re-import",
                    wf.name,
                    wf.content_hash
                );
            }
            return Ok(existing);
        }
        if wf.catalog_default {
            let other_default: Option<String> = tx
                .query_row(
                    "SELECT catalog_id FROM workflow_versions WHERE catalog_default = 1 AND content_hash != ?1 LIMIT 1",
                    params![wf.content_hash],
                    |row| row.get(0),
                )
                .ok();
            if let Some(existing) = other_default {
                anyhow::bail!(
                    "catalog '{}' is already the explicit default; at most one catalog default is allowed (remove catalog.default from one workflow)",
                    existing
                );
            }
        }
        let next_rev: i64 = if let Some(catalog_id) = wf.catalog_id.as_deref() {
            tx.query_row(
                "SELECT COALESCE(MAX(revision), 0) + 1 FROM workflow_versions WHERE catalog_id = ?1",
                params![catalog_id],
                |r| r.get(0),
            )?
        } else {
            tx.query_row(
                "SELECT COALESCE(MAX(revision), 0) + 1 FROM workflow_versions WHERE name = ?1",
                params![wf.name],
                |r| r.get(0),
            )?
        };
        let mut out = wf.clone();
        out.revision = next_rev;
        tx.execute(
            "INSERT INTO workflow_versions (id, name, smg_path, version, revision, description, content_hash, content_json, entry, exit, flow_count, gate_count, stage_count, catalog_id, catalog_label, catalog_description, catalog_default, capability_schema_version, capability_hash, source_id, source_rel_path, source_kind, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)",
            params![out.id, out.name, out.smg_path, out.version, out.revision, out.description, out.content_hash, out.content_json, out.entry, out.exit, out.flow_count, out.gate_count, out.stage_count, out.catalog_id, out.catalog_label, out.catalog_description, out.catalog_default, out.capability_schema_version, out.capability_hash, out.source_id, out.source_rel_path, out.source_kind, out.created_at],
        ).with_context(|| format!("Failed to register workflow version for '{}'", wf.name))?;
        for asset in assets {
            tx.execute(
                "INSERT INTO workflow_assets (workflow_version_id, logical_path, content_hash, content)
                 VALUES (?1, ?2, ?3, ?4)",
                params![out.id, asset.logical_path, asset.content_hash, asset.content],
            )
            .with_context(|| {
                format!(
                    "Failed to persist package asset '{}' for workflow '{}'",
                    asset.logical_path, wf.name
                )
            })?;
        }
        tx.commit()?;
        tracing::info!(workflow_id = %out.id, name = %out.name, revision = out.revision, "Registered workflow version");
        Ok(out)
    }

    pub fn workflow_by_hash(&self, hash: &str) -> Result<Option<WorkflowVersion>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare("SELECT id, name, smg_path, version, revision, description, content_hash, content_json, entry, exit, flow_count, gate_count, stage_count, catalog_id, catalog_label, catalog_description, catalog_default, capability_schema_version, capability_hash, source_id, source_rel_path, source_kind, created_at FROM workflow_versions WHERE content_hash = ?1")?;
        let mut rows = stmt.query(params![hash])?;
        match rows.next()? {
            Some(row) => Ok(Some(Self::read_workflow_row(row)?)),
            None => Ok(None),
        }
    }

    pub fn get_workflow(&self, id: &str) -> Result<Option<WorkflowVersion>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare("SELECT id, name, smg_path, version, revision, description, content_hash, content_json, entry, exit, flow_count, gate_count, stage_count, catalog_id, catalog_label, catalog_description, catalog_default, capability_schema_version, capability_hash, source_id, source_rel_path, source_kind, created_at FROM workflow_versions WHERE id = ?1")?;
        let mut rows = stmt.query(params![id])?;
        match rows.next()? {
            Some(row) => Ok(Some(Self::read_workflow_row(row)?)),
            None => Ok(None),
        }
    }

    pub fn list_workflows(&self) -> Result<Vec<WorkflowVersion>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare("SELECT id, name, smg_path, version, revision, description, content_hash, content_json, entry, exit, flow_count, gate_count, stage_count, catalog_id, catalog_label, catalog_description, catalog_default, capability_schema_version, capability_hash, source_id, source_rel_path, source_kind, created_at FROM workflow_versions ORDER BY name, revision DESC")?;
        let rows = stmt.query_map([], Self::read_workflow_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    fn read_workflow_row(row: &rusqlite::Row) -> rusqlite::Result<WorkflowVersion> {
        Ok(WorkflowVersion {
            id: row.get(0)?,
            name: row.get(1)?,
            smg_path: row.get(2)?,
            version: row.get(3)?,
            revision: row.get(4)?,
            description: row.get(5)?,
            content_hash: row.get(6)?,
            content_json: row.get(7)?,
            entry: row.get(8)?,
            exit: row.get(9)?,
            flow_count: row.get(10)?,
            gate_count: row.get(11)?,
            stage_count: row.get(12)?,
            catalog_id: row.get(13).unwrap_or(None),
            catalog_label: row.get(14).unwrap_or(None),
            catalog_description: row.get(15).unwrap_or(None),
            catalog_default: row.get::<_, i64>(16).unwrap_or(0) != 0,
            capability_schema_version: row.get::<_, i64>(17).unwrap_or(0),
            capability_hash: row.get(18).unwrap_or(None),
            source_id: row.get(19).unwrap_or(None),
            source_rel_path: row.get(20).unwrap_or(None),
            source_kind: row.get(21).unwrap_or(None),
            created_at: row.get(22)?,
        })
    }

    fn read_workflow_assets(
        conn: &rusqlite::Connection,
        workflow_version_id: &str,
    ) -> Result<Vec<WorkflowAsset>> {
        let mut stmt = conn.prepare(
            "SELECT logical_path, content_hash, content
             FROM workflow_assets WHERE workflow_version_id = ?1
             ORDER BY logical_path",
        )?;
        let rows = stmt.query_map(params![workflow_version_id], |row| {
            Ok(WorkflowAsset {
                logical_path: row.get(0)?,
                content_hash: row.get(1)?,
                content: row.get(2)?,
            })
        })?;
        let mut assets = Vec::new();
        for row in rows {
            assets.push(row?);
        }
        Ok(assets)
    }

    /// Stored package assets in logical-path order. Empty for legacy graphs.
    pub fn workflow_assets(&self, workflow_version_id: &str) -> Result<Vec<WorkflowAsset>> {
        let conn = lock_conn!(self);
        Self::read_workflow_assets(&conn, workflow_version_id)
    }

    pub fn workflow_asset_count(&self, workflow_version_id: &str) -> Result<i64> {
        let conn = lock_conn!(self);
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM workflow_assets WHERE workflow_version_id = ?1",
            params![workflow_version_id],
            |row| row.get(0),
        )?)
    }

    /// Latest immutable revision for each catalog id, ordered by label. This is
    /// the only source for `/v1/profiles` and project selection.
    pub fn list_catalog(&self) -> Result<Vec<WorkflowVersion>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(
            "SELECT w.id, w.name, w.smg_path, w.version, w.revision, w.description,
                    w.content_hash, w.content_json, w.entry, w.exit, w.flow_count,
                    w.gate_count, w.stage_count, w.catalog_id, w.catalog_label,
                    w.catalog_description, w.catalog_default,
                    w.capability_schema_version, w.capability_hash, w.source_id,
                    w.source_rel_path, w.source_kind, w.created_at
             FROM workflow_versions w
             WHERE w.catalog_id IS NOT NULL
               AND w.revision = (
                   SELECT MAX(latest.revision) FROM workflow_versions latest
                   WHERE latest.catalog_id = w.catalog_id
               )
             ORDER BY COALESCE(w.catalog_label, w.name), w.catalog_id",
        )?;
        let rows = stmt.query_map([], Self::read_workflow_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn latest_workflow_for_catalog(&self, catalog_id: &str) -> Result<Option<WorkflowVersion>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(
            "SELECT id, name, smg_path, version, revision, description, content_hash,
                    content_json, entry, exit, flow_count, gate_count, stage_count,
                    catalog_id, catalog_label, catalog_description, catalog_default,
                    capability_schema_version, capability_hash, source_id,
                    source_rel_path, source_kind, created_at
             FROM workflow_versions WHERE catalog_id = ?1
             ORDER BY revision DESC LIMIT 1",
        )?;
        let mut rows = stmt.query(params![catalog_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(Self::read_workflow_row(row)?)),
            None => Ok(None),
        }
    }

    pub fn catalog_default_workflow(&self) -> Result<Option<WorkflowVersion>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(
            "SELECT id, name, smg_path, version, revision, description, content_hash,
                    content_json, entry, exit, flow_count, gate_count, stage_count,
                    catalog_id, catalog_label, catalog_description, catalog_default,
                    capability_schema_version, capability_hash, source_id,
                    source_rel_path, source_kind, created_at
             FROM workflow_versions WHERE catalog_default = 1
             ORDER BY revision DESC LIMIT 1",
        )?;
        let mut rows = stmt.query([])?;
        match rows.next()? {
            Some(row) => Ok(Some(Self::read_workflow_row(row)?)),
            None => Ok(None),
        }
    }

    // ---- Immutable execution bundles (TJ-012) ----

    /// Persist the immutable bundle for a run. Re-saving identical content is
    /// idempotent; a different bundle for the same run is a hard conflict.
    pub fn save_execution_bundle(
        &self,
        bundle: &ExecutionBundleRecord,
    ) -> Result<ExecutionBundleRecord> {
        let conn = lock_conn!(self);
        conn.execute(
            "INSERT INTO execution_bundles (id, run_id, workflow_id, bundle_hash, bundle_json, legacy, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(run_id) DO NOTHING",
            params![
                bundle.id,
                bundle.run_id.as_str(),
                bundle.workflow_id,
                bundle.bundle_hash,
                bundle.bundle_json,
                bundle.legacy as i64,
                bundle.created_at
            ],
        ).with_context(|| format!("Failed to persist execution bundle for run {}", bundle.run_id.as_str()))?;
        let existing = {
            let mut stmt = conn.prepare(
                "SELECT id, run_id, workflow_id, bundle_hash, bundle_json, legacy, created_at
                 FROM execution_bundles WHERE run_id = ?1",
            )?;
            let mut rows = stmt.query(params![bundle.run_id.as_str()])?;
            match rows.next()? {
                Some(row) => ExecutionBundleRecord {
                    id: row.get(0)?,
                    run_id: RunId(row.get(1)?),
                    workflow_id: row.get(2)?,
                    bundle_hash: row.get(3)?,
                    bundle_json: row.get(4)?,
                    legacy: row.get::<_, i64>(5)? != 0,
                    created_at: row.get(6)?,
                },
                None => anyhow::bail!(
                    "execution bundle for run '{}' disappeared after insert",
                    bundle.run_id.as_str()
                ),
            }
        };
        anyhow::ensure!(
            existing.bundle_hash == bundle.bundle_hash && existing.run_id == bundle.run_id,
            "run '{}' already has a different immutable execution bundle",
            bundle.run_id.as_str()
        );
        Ok(existing)
    }

    pub fn delete_execution_bundle_for_run(&self, run_id: &str) -> Result<()> {
        let conn = lock_conn!(self);
        conn.execute(
            "DELETE FROM execution_bundles WHERE run_id = ?1",
            params![run_id],
        )?;
        Ok(())
    }

    pub fn execution_bundle_for_run(&self, run_id: &str) -> Result<Option<ExecutionBundleRecord>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(
            "SELECT id, run_id, workflow_id, bundle_hash, bundle_json, legacy, created_at
             FROM execution_bundles WHERE run_id = ?1",
        )?;
        let mut rows = stmt.query(params![run_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(ExecutionBundleRecord {
                id: row.get(0)?,
                run_id: RunId(row.get(1)?),
                workflow_id: row.get(2)?,
                bundle_hash: row.get(3)?,
                bundle_json: row.get(4)?,
                legacy: row.get::<_, i64>(5)? != 0,
                created_at: row.get(6)?,
            })),
            None => Ok(None),
        }
    }

    /// Finalizes stages left in `waiting_for_input` (e.g. a restart killed the
    /// conversation mid-wait and the resume created fresh stage rows). Returns
    /// how many rows were finalized.
    pub fn finalize_stale_waiting_stages(&self, run_id: &str, reason: &str) -> Result<u64> {
        let mut conn = lock_conn!(self);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let n = lifecycle::finalize_rows(
            &tx,
            "stages",
            run_id,
            reason,
            "status = 'waiting_for_input' AND flow_id IN (SELECT id FROM flows WHERE run_id = ?1)",
        )?;
        questions::close_open_for_run_tx(&tx, run_id, reason)?;
        tx.commit()?;
        if n > 0 {
            tracing::info!(run_id = %run_id, finalized = n, "Finalized stale waiting_for_input stages");
        }
        Ok(n as u64)
    }

    /// Marks superseded flow rows failed: when a run resumes it creates a new
    /// row for the same flow name, leaving the old row dangling. The latest
    /// row per name stays untouched.
    pub fn finalize_stale_flow_rows(&self, run_id: &str) -> Result<u64> {
        let mut conn = lock_conn!(self);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let n = lifecycle::finalize_rows(
            &tx,
            "flows",
            run_id,
            "superseded flow",
            "run_id = ?1 AND status IN ('pending','running') AND rowid != (SELECT f2.rowid FROM flows f2 WHERE f2.run_id = flows.run_id AND f2.flow_name = flows.flow_name ORDER BY f2.rowid DESC LIMIT 1)",
        )?;
        tx.commit()?;
        if n > 0 {
            tracing::info!(run_id = %run_id, finalized = n, "Finalized superseded flow rows");
        }
        Ok(n as u64)
    }

    /// Deletes a run and every child record. Call only for terminal runs —
    /// the caller owns that policy.
    pub fn delete_run_cascade(&self, run_id: &str) -> Result<()> {
        let conn = lock_conn!(self);
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM stages WHERE flow_id IN (SELECT id FROM flows WHERE run_id = ?1)",
            params![run_id],
        )?;
        tx.execute("DELETE FROM flows WHERE run_id = ?1", params![run_id])?;
        tx.execute(
            "DELETE FROM execution_attempts WHERE run_id = ?1",
            params![run_id],
        )?;
        tx.execute("DELETE FROM stage_logs WHERE run_id = ?1", params![run_id])?;
        tx.execute("DELETE FROM artifacts WHERE run_id = ?1", params![run_id])?;
        tx.execute("DELETE FROM events WHERE run_id = ?1", params![run_id])?;
        tx.execute(
            "DELETE FROM conversations WHERE run_id = ?1",
            params![run_id],
        )?;
        tx.execute("DELETE FROM questions WHERE run_id = ?1", params![run_id])?;
        tx.execute(
            "DELETE FROM gate_decisions WHERE run_id = ?1",
            params![run_id],
        )?;
        tx.execute(
            "DELETE FROM run_gate_outputs WHERE run_id = ?1",
            params![run_id],
        )?;
        tx.execute(
            "DELETE FROM execution_bundles WHERE run_id = ?1",
            params![run_id],
        )?;
        let n = tx.execute("DELETE FROM pipeline_runs WHERE id = ?1", params![run_id])?;
        tx.commit()?;
        if n == 0 {
            anyhow::bail!("run '{}' does not exist", run_id);
        }
        tracing::info!(run_id = %run_id, "Run deleted (cascade)");
        Ok(())
    }

    /// Pins a lane to a workflow version (None = follow the configured SMG).
    pub fn set_lane_workflow(&self, lane_id: &str, workflow_id: Option<&str>) -> Result<()> {
        let conn = lock_conn!(self);
        let n = conn
            .execute(
                "UPDATE lanes SET workflow_id = ?1 WHERE id = ?2",
                params![workflow_id, lane_id],
            )
            .with_context(|| format!("Failed to pin lane {}", lane_id))?;
        if n == 0 {
            anyhow::bail!("lane '{}' does not exist", lane_id);
        }
        Ok(())
    }

    pub fn lanes_using_workflow(&self, workflow_id: &str) -> Result<Vec<Lane>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare("SELECT id, project_id, name, smg_path, description, workflow_id, created_at FROM lanes WHERE workflow_id = ?1 ORDER BY rowid")?;
        let rows = stmt.query_map(params![workflow_id], Self::read_lane_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn save_conversation(&self, conv: &ConversationRecord) -> Result<()> {
        let conn = lock_conn!(self);
        conn.execute(
            "INSERT OR REPLACE INTO conversations (id, run_id, conversation_type, provider, provider_key, stage_name, session_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![conv.id, conv.run_id.as_str(), conv.conversation_type, conv.provider, conv.provider_key, conv.stage_name, conv.session_id, conv.created_at],
        ).with_context(|| format!("Failed to save conversation {}", conv.id))?;
        Ok(())
    }

    pub fn load_conversation(&self, id: &str) -> Result<Option<ConversationRecord>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(
            "SELECT id, run_id, conversation_type, provider, provider_key, stage_name, session_id, created_at FROM conversations WHERE id = ?1"
        )?;
        let mut rows = stmt.query(params![id])?;
        match rows.next()? {
            Some(row) => Ok(Some(ConversationRecord {
                id: row.get(0)?,
                run_id: RunId(row.get(1)?),
                conversation_type: row.get(2)?,
                provider: row.get(3)?,
                provider_key: row.get(4)?,
                stage_name: row.get(5)?,
                session_id: row.get(6)?,
                created_at: row.get(7)?,
            })),
            None => Ok(None),
        }
    }

    pub fn load_conversation_by_provider_key(
        &self,
        provider_key: &str,
    ) -> Result<Option<ConversationRecord>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(
            "SELECT id, run_id, conversation_type, provider, provider_key, stage_name, session_id, created_at FROM conversations WHERE provider_key = ?1"
        )?;
        let mut rows = stmt.query(params![provider_key])?;
        match rows.next()? {
            Some(row) => Ok(Some(ConversationRecord {
                id: row.get(0)?,
                run_id: RunId(row.get(1)?),
                conversation_type: row.get(2)?,
                provider: row.get(3)?,
                provider_key: row.get(4)?,
                stage_name: row.get(5)?,
                session_id: row.get(6)?,
                created_at: row.get(7)?,
            })),
            None => Ok(None),
        }
    }

    pub fn list_conversations_for_run(&self, run_id: &str) -> Result<Vec<ConversationRecord>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(
            "SELECT id, run_id, conversation_type, provider, provider_key, stage_name, session_id, created_at FROM conversations WHERE run_id = ?1 ORDER BY created_at"
        )?;
        let rows = stmt.query_map(params![run_id], |row| {
            Ok(ConversationRecord {
                id: row.get(0)?,
                run_id: RunId(row.get(1)?),
                conversation_type: row.get(2)?,
                provider: row.get(3)?,
                provider_key: row.get(4)?,
                stage_name: row.get(5)?,
                session_id: row.get(6)?,
                created_at: row.get(7)?,
            })
        })?;
        let mut convs = Vec::new();
        for row in rows {
            convs.push(row?);
        }
        Ok(convs)
    }

    pub fn load_conversation_by_stage(
        &self,
        run_id: &str,
        stage_name: &str,
    ) -> Result<Option<ConversationRecord>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(
            "SELECT id, run_id, conversation_type, provider, provider_key, stage_name, session_id, created_at FROM conversations WHERE run_id = ?1 AND stage_name = ?2 AND conversation_type = 'stage' ORDER BY created_at DESC LIMIT 1"
        )?;
        let mut rows = stmt.query(params![run_id, stage_name])?;
        match rows.next()? {
            Some(row) => Ok(Some(ConversationRecord {
                id: row.get(0)?,
                run_id: RunId(row.get(1)?),
                conversation_type: row.get(2)?,
                provider: row.get(3)?,
                provider_key: row.get(4)?,
                stage_name: row.get(5)?,
                session_id: row.get(6)?,
                created_at: row.get(7)?,
            })),
            None => Ok(None),
        }
    }

    pub fn next_flow_number(&self, project_id: &str, flow_name: &str) -> Result<u64> {
        let conn = lock_conn!(self);
        conn.execute(
            "INSERT OR IGNORE INTO flow_counters (project_id, flow_name, next_number) VALUES (?1, ?2, 1)",
            params![project_id, flow_name],
        )?;
        let number: u64 = conn.query_row(
            "SELECT next_number FROM flow_counters WHERE project_id = ?1 AND flow_name = ?2",
            params![project_id, flow_name],
            |row| row.get(0),
        )?;
        conn.execute(
            "UPDATE flow_counters SET next_number = next_number + 1 WHERE project_id = ?1 AND flow_name = ?2",
            params![project_id, flow_name],
        )?;
        Ok(number)
    }

    pub fn load_conversation_by_flow(
        &self,
        flow_name: &str,
        flow_number: u64,
    ) -> Result<Option<ConversationRecord>> {
        let provider_key_prefix = format!(
            "{}::{} #{}",
            flow_name.to_lowercase(),
            flow_name,
            flow_number
        );
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(
            "SELECT id, run_id, conversation_type, provider, provider_key, stage_name, session_id, created_at FROM conversations WHERE provider_key LIKE ?1 || '%' AND conversation_type = 'flow' ORDER BY created_at DESC LIMIT 1"
        )?;
        let mut rows = stmt.query(params![provider_key_prefix])?;
        match rows.next()? {
            Some(row) => Ok(Some(ConversationRecord {
                id: row.get(0)?,
                run_id: RunId(row.get(1)?),
                conversation_type: row.get(2)?,
                provider: row.get(3)?,
                provider_key: row.get(4)?,
                stage_name: row.get(5)?,
                session_id: row.get(6)?,
                created_at: row.get(7)?,
            })),
            None => Ok(None),
        }
    }

    pub fn complete_flow(&self, flow_id: &str) -> Result<()> {
        let mut flow = self
            .load_flow(flow_id)?
            .context("Flow disappeared before completion")?;
        flow.status = FlowStatus::Completed;
        flow.completed_at = Some(chrono::Utc::now().to_rfc3339());
        self.save_flow(&flow)
    }

    pub fn update_flow_status(&self, flow_id: &str, status: FlowStatus) -> Result<()> {
        let mut flow = self
            .load_flow(flow_id)?
            .context("Flow disappeared before transition")?;
        flow.status = status;
        if matches!(
            flow.status,
            FlowStatus::Completed | FlowStatus::Failed | FlowStatus::TimedOut
        ) {
            flow.completed_at = Some(chrono::Utc::now().to_rfc3339());
        }
        self.save_flow(&flow)
    }

    pub fn save_secret(&self, project_id: &str, key: &str, encrypted_value: &str) -> Result<()> {
        let conn = lock_conn!(self);
        tracing::debug!(project_id = %project_id, key = %key, "Saving secret");
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT OR REPLACE INTO project_secrets (project_id, key, encrypted_value, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![project_id, key, encrypted_value, now, now],
        ).with_context(|| format!("Failed to save secret {} for project {}", key, project_id))?;
        Ok(())
    }

    pub fn load_secret(&self, project_id: &str, key: &str) -> Result<Option<String>> {
        let conn = lock_conn!(self);
        tracing::debug!(project_id = %project_id, key = %key, "Loading secret");
        let mut stmt = conn.prepare(
            "SELECT encrypted_value FROM project_secrets WHERE project_id = ?1 AND key = ?2",
        )?;
        let mut rows = stmt.query(params![project_id, key])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    pub fn delete_secret(&self, project_id: &str, key: &str) -> Result<()> {
        let conn = lock_conn!(self);
        tracing::debug!(project_id = %project_id, key = %key, "Deleting secret");
        conn.execute(
            "DELETE FROM project_secrets WHERE project_id = ?1 AND key = ?2",
            params![project_id, key],
        )
        .with_context(|| format!("Failed to delete secret {} for project {}", key, project_id))?;
        Ok(())
    }

    pub fn delete_secrets_for_project(&self, project_id: &str) -> Result<()> {
        let conn = lock_conn!(self);
        tracing::debug!(project_id = %project_id, "Deleting all secrets for project");
        conn.execute(
            "DELETE FROM project_secrets WHERE project_id = ?1",
            params![project_id],
        )
        .with_context(|| format!("Failed to delete secrets for project {}", project_id))?;
        Ok(())
    }

    pub fn list_secrets(&self, project_id: &str) -> Result<Vec<String>> {
        let conn = lock_conn!(self);
        tracing::debug!(project_id = %project_id, "Listing secrets for project");
        let mut stmt =
            conn.prepare("SELECT key FROM project_secrets WHERE project_id = ?1 ORDER BY key")?;
        let rows = stmt.query_map(params![project_id], |row| row.get(0))?;
        let mut keys = Vec::new();
        for row in rows {
            keys.push(row?);
        }
        tracing::debug!(project_id = %project_id, count = keys.len(), "Listed secrets");
        Ok(keys)
    }

    pub fn create_user(&self, user: &User) -> Result<()> {
        let conn = lock_conn!(self);
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO users (id, username, password_hash, display_name, is_admin, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![user.id, user.username, user.password_hash, user.display_name, user.is_admin as i64, now],
        )?;
        Ok(())
    }

    pub fn count_users(&self) -> Result<u64> {
        let conn = lock_conn!(self);
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))?;
        Ok(n as u64)
    }

    pub fn get_user_by_username(&self, username: &str) -> Result<Option<User>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(
            "SELECT id, username, password_hash, display_name, is_admin, created_at FROM users WHERE username = ?1",
        )?;
        let mut rows = stmt.query(params![username])?;
        match rows.next()? {
            Some(row) => Ok(Some(User {
                id: row.get(0)?,
                username: row.get(1)?,
                password_hash: row.get(2)?,
                display_name: row.get(3)?,
                is_admin: row.get::<_, i64>(4)? != 0,
                created_at: row.get(5)?,
            })),
            None => Ok(None),
        }
    }

    pub fn list_users(&self) -> Result<Vec<User>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(
            "SELECT id, username, password_hash, display_name, is_admin, created_at FROM users ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(User {
                id: row.get(0)?,
                username: row.get(1)?,
                password_hash: row.get(2)?,
                display_name: row.get(3)?,
                is_admin: row.get::<_, i64>(4)? != 0,
                created_at: row.get(5)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn issue_token(&self, token: &str, user_id: &str) -> Result<()> {
        let conn = lock_conn!(self);
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT OR REPLACE INTO auth_tokens (token, user_id, created_at, last_used_at) VALUES (?1, ?2, ?3, ?3)",
            params![token, user_id, now],
        )?;
        Ok(())
    }

    /// Sessions have an absolute 30-day lifetime from token issuance, not
    /// account creation or last use. Invalid/future timestamps fail closed.
    pub fn user_for_token(&self, token: &str) -> Result<Option<User>> {
        self.user_for_token_at(token, chrono::Utc::now())
    }

    fn user_for_token_at(
        &self,
        token: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<User>> {
        let conn = lock_conn!(self);
        let session = {
            let mut stmt = conn.prepare(
                "SELECT u.id, u.username, u.password_hash, u.display_name, u.is_admin, u.created_at, t.created_at FROM auth_tokens t JOIN users u ON u.id = t.user_id WHERE t.token = ?1",
            )?;
            let mut rows = stmt.query(params![token])?;
            match rows.next()? {
                Some(row) => Some((
                    User {
                        id: row.get(0)?,
                        username: row.get(1)?,
                        password_hash: row.get(2)?,
                        display_name: row.get(3)?,
                        is_admin: row.get::<_, i64>(4)? != 0,
                        created_at: row.get(5)?,
                    },
                    row.get::<_, Option<String>>(6)?,
                )),
                None => None,
            }
        };
        let Some((user, issued_at)) = session else {
            return Ok(None);
        };
        let valid = issued_at
            .as_deref()
            .and_then(|issued| chrono::DateTime::parse_from_rfc3339(issued).ok())
            .map(|issued| {
                let age = now.signed_duration_since(issued);
                age >= chrono::Duration::zero() && age < chrono::Duration::days(30)
            })
            .unwrap_or(false);
        if !valid {
            conn.execute("DELETE FROM auth_tokens WHERE token = ?1", params![token])?;
            return Ok(None);
        }
        conn.execute(
            "UPDATE auth_tokens SET last_used_at = ?2 WHERE token = ?1",
            params![token, now.to_rfc3339()],
        )?;
        Ok(Some(user))
    }

    pub fn revoke_token(&self, token: &str) -> Result<()> {
        let conn = lock_conn!(self);
        conn.execute("DELETE FROM auth_tokens WHERE token = ?1", params![token])?;
        Ok(())
    }

    /// Persist the gate identity and waiting state as one optimistic lifecycle
    /// transition. A gate id is single-use and unique across runs.
    pub fn enter_gate(
        &self,
        run_id: &str,
        expected_state_version: i64,
        gate_instance_id: &str,
        gate_name: &str,
        summary: &str,
    ) -> Result<i64> {
        self.enter_gate_bound(
            run_id,
            expected_state_version,
            gate_instance_id,
            gate_name,
            summary,
            None,
        )
    }

    /// Enter a gate bound to one exact packet revision. The binding is written
    /// in the same transaction as the waiting-state transition, so an approval
    /// can never reference content the reviewer did not see.
    pub fn enter_gate_bound(
        &self,
        run_id: &str,
        expected_state_version: i64,
        gate_instance_id: &str,
        gate_name: &str,
        summary: &str,
        packet: Option<&packets::PacketRevision>,
    ) -> Result<i64> {
        self.enter_gate_bound_to(
            run_id,
            expected_state_version,
            gate_instance_id,
            gate_name,
            summary,
            packet,
            None,
        )
    }

    /// Enter a gate bound to one exact plan revision (and its source packet).
    /// The plan and packet bindings are written in the same transaction as the
    /// waiting-state transition, so approval can never reference content the
    /// reviewer did not see.
    pub fn enter_gate_bound_to(
        &self,
        run_id: &str,
        expected_state_version: i64,
        gate_instance_id: &str,
        gate_name: &str,
        summary: &str,
        packet: Option<&packets::PacketRevision>,
        plan: Option<&plans::PlanRevision>,
    ) -> Result<i64> {
        self.enter_gate_bound_to_output(
            run_id,
            expected_state_version,
            gate_instance_id,
            gate_name,
            summary,
            packet,
            plan,
            None,
        )
    }

    /// Enter a gate bound to one exact generic output artifact. Packet/plan
    /// bindings remain mutually exclusive with this artifact binding.
    pub fn enter_gate_bound_to_output(
        &self,
        run_id: &str,
        expected_state_version: i64,
        gate_instance_id: &str,
        gate_name: &str,
        summary: &str,
        packet: Option<&packets::PacketRevision>,
        plan: Option<&plans::PlanRevision>,
        output: Option<&RunArtifact>,
    ) -> Result<i64> {
        anyhow::ensure!(
            output.is_none() || (packet.is_none() && plan.is_none()),
            "A generic output gate cannot also bind a packet or plan"
        );
        anyhow::ensure!(
            !gate_instance_id.trim().is_empty(),
            "gate instance id is required"
        );
        anyhow::ensure!(!gate_name.trim().is_empty(), "gate name is required");
        let mut conn = lock_conn!(self);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let revision = lifecycle::Revision::loaded(expected_state_version);
        lifecycle::guard_write(&tx, "pipeline_runs", run_id, &revision, "waiting_for_gate")?;
        let changed = tx.execute(
            "UPDATE pipeline_runs SET status='waiting_for_gate', gate_state=?1, gate_instance_id=?2,
                 gate_name=?3, updated_at=?4 WHERE id=?5 AND revision=?6 AND status='running'",
            params![summary, gate_instance_id, gate_name, chrono::Utc::now().to_rfc3339(), run_id, expected_state_version + 1],
        )?;
        anyhow::ensure!(changed == 1, "Gate state conflict for run {run_id}");
        if let Some(packet) = packet {
            anyhow::ensure!(
                packet.run_id == run_id,
                "Packet revision belongs to another run"
            );
            packets::bind_gate_packet_tx(&tx, run_id, gate_instance_id, packet)?;
        }
        if let Some(plan) = plan {
            anyhow::ensure!(
                plan.run_id == run_id,
                "Plan revision belongs to another run"
            );
            if let Some(packet) = packet {
                anyhow::ensure!(
                    plan.packet_revision_id == packet.id
                        && plan.packet_content_hash == packet.content_hash,
                    "Plan revision was generated from a different packet revision than the one bound to this gate"
                );
            }
            plans::bind_gate_plan_tx(&tx, run_id, gate_instance_id, plan)?;
        }
        if let Some(output) = output {
            outputs::bind_gate_output_tx(&tx, run_id, gate_instance_id, output)?;
        }
        lifecycle::finish_write(
            &tx,
            "pipeline_runs",
            run_id,
            run_id,
            "waiting_for_gate",
            &revision,
        )?;
        tx.execute(
            "INSERT INTO events (id, run_id, kind, flow_name, summary, payload, created_at)
             VALUES (?1, ?2, 'gate_reached', ?3, ?4, ?5, ?6)",
            params![format!("evt_{}", uuid::Uuid::new_v4()), run_id, gate_name, summary,
                serde_json::json!({"gate_instance_id":gate_instance_id,"state_version":expected_state_version + 1,
                    "packet_id": packet.map(|p| p.packet_id.clone()),
                    "packet_revision_id": packet.map(|p| p.id.clone()),
                    "packet_content_hash": packet.map(|p| p.content_hash.clone()),
                    "packet_completeness": packet.map(|p| p.completeness.clone()),
                    "plan_id": plan.map(|p| p.plan_id.clone()),
                    "plan_revision_id": plan.map(|p| p.id.clone()),
                    "plan_content_hash": plan.map(|p| p.content_hash.clone()),
                    "plan_task_count": plan.map(|p| serde_json::from_str::<serde_json::Value>(&p.validation_json)
                        .ok().and_then(|v| v.get("task_count").and_then(|v| v.as_u64()))),
                    "output_artifact_id": output.map(|a| a.id.clone()),
                    "output_content_hash": output.map(|a| a.content_hash.clone()),
                    "output_stage_name": output.map(|a| a.stage_name.clone()),
                    "output_flow_name": output.map(|a| a.flow_name.clone()) }).to_string(),
                chrono::Utc::now().to_rfc3339()],
        )?;
        tx.commit()?;
        Ok(expected_state_version + 1)
    }

    /// Atomically accept a gate decision only for the exact active gate and
    /// run state version. The decision row, run transition, and audit events
    /// share one SQLite transaction. An idempotent retry returns its original
    /// receipt; any stale/forged request has no effects.
    pub fn decide_gate(
        &self,
        d: &GateDecision,
        expected_state_version: i64,
        rework_limit: usize,
    ) -> Result<GateDecisionReceipt> {
        let gate_instance_id = d
            .gate_instance_id
            .as_deref()
            .context("gate instance id is required")?;
        let idempotency_key = d
            .idempotency_key
            .as_deref()
            .context("idempotency key is required")?;
        anyhow::ensure!(
            !gate_instance_id.trim().is_empty(),
            "gate instance id is required"
        );
        anyhow::ensure!(
            !idempotency_key.trim().is_empty()
                && idempotency_key.len() <= 200
                && !idempotency_key.chars().any(char::is_control),
            "idempotency key must contain 1–200 printable characters"
        );
        anyhow::ensure!(
            !d.actor_id.trim().is_empty(),
            "gate decision actor id is required"
        );

        let mut conn = lock_conn!(self);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let authorized: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM users u JOIN pipeline_runs r ON r.id=?2
             WHERE u.id=?1 AND (u.is_admin=1 OR EXISTS(SELECT 1 FROM project_members m
                 JOIN projects p ON p.id=m.project_id WHERE m.project_id=r.project_id AND m.user_id=u.id)))",
            params![d.actor_id, d.run_id.as_str()], |row| row.get(0),
        )?;
        anyhow::ensure!(authorized, "Project access denied");

        let existing: Option<(String, String, String, i64, Option<String>, Option<String>, Option<String>, Option<i64>, Option<i64>, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>)> = tx.query_row(
            "SELECT id, run_id, gate_instance_id, approved, reason, actor_id, note, expected_state_version, state_version, disposition,
                    packet_revision_id, packet_content_hash, plan_revision_id, plan_content_hash,
                    output_artifact_id, output_content_hash, output_stage_name
             FROM gate_decisions WHERE idempotency_key=?1",
            params![idempotency_key],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?, row.get(10)?, row.get(11)?, row.get(12)?, row.get(13)?, row.get(14)?, row.get(15)?, row.get(16)?)),
        ).optional()?;
        if let Some((
            id,
            prior_run,
            prior_gate,
            prior_approved,
            prior_reason,
            prior_actor,
            prior_note,
            prior_expected,
            prior_version,
            prior_disposition,
            prior_packet_revision,
            prior_packet_hash,
            prior_plan_revision,
            prior_plan_hash,
            prior_output_artifact,
            prior_output_hash,
            prior_output_stage,
        )) = existing
        {
            anyhow::ensure!(
                prior_run == d.run_id.as_str()
                    && prior_gate == gate_instance_id
                    && prior_approved == d.approved as i64
                    && prior_reason == d.reason
                    && prior_actor.as_deref() == Some(d.actor_id.as_str())
                    && prior_note == d.note
                    && prior_expected == Some(expected_state_version)
                    && prior_packet_revision == d.packet_revision_id
                    && prior_packet_hash == d.packet_content_hash
                    && prior_plan_revision == d.plan_revision_id
                    && prior_plan_hash == d.plan_content_hash
                    && prior_output_artifact == d.output_artifact_id
                    && prior_output_hash == d.output_content_hash
                    && prior_output_stage == d.output_stage_name,
                "Idempotency key is already bound to a different gate decision"
            );
            let disposition =
                prior_disposition.context("accepted gate decision has no disposition")?;
            let state_version =
                prior_version.context("accepted gate decision has no resulting state version")?;
            let status = if disposition == "rework_limit_cancelled" {
                RunStatus::Cancelled
            } else {
                RunStatus::Running
            };
            let effect_id = tx
                .query_row(
                    "SELECT id FROM pending_effects WHERE idempotency_key = ?1",
                    params![format!("gate_route:{id}")],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            let gate_binding_kind = if prior_plan_revision.is_some() {
                Some("plan".to_string())
            } else if prior_packet_revision.is_some() {
                Some("packet".to_string())
            } else if prior_output_artifact.is_some() {
                Some("artifact".to_string())
            } else {
                None
            };
            return Ok(GateDecisionReceipt {
                id,
                run_id: prior_run,
                gate_instance_id: prior_gate,
                approved: prior_approved != 0,
                status,
                state_version,
                disposition,
                replayed: true,
                packet_revision_id: prior_packet_revision,
                packet_content_hash: prior_packet_hash,
                plan_revision_id: prior_plan_revision,
                plan_content_hash: prior_plan_hash,
                output_artifact_id: prior_output_artifact,
                output_content_hash: prior_output_hash,
                output_stage_name: prior_output_stage,
                gate_binding_kind,
                effect_id,
            });
        }

        let current: Option<(String, Option<String>, Option<String>, i64)> = tx.query_row(
            "SELECT status, gate_instance_id, gate_name, revision FROM pipeline_runs WHERE id=?1",
            params![d.run_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        ).optional()?;
        let Some((status, active_gate, active_gate_name, actual_version)) = current else {
            anyhow::bail!("Run not found");
        };
        anyhow::ensure!(
            status == "waiting_for_gate" && active_gate.as_deref() == Some(gate_instance_id),
            "Gate conflict: submitted gate instance is not active"
        );
        let gate_name = active_gate_name.context("Gate conflict: active gate has no name")?;
        anyhow::ensure!(
            actual_version == expected_state_version,
            "Gate conflict: expected state version {expected_state_version}, current version is {actual_version}"
        );

        // A gate may be bound to one exact packet revision. An approval or
        // change request from a stale review must conflict before any writes.
        let packet_binding: Option<(String, String, i64, String, String)> = tx
            .query_row(
                "SELECT packet_id, revision_id, revision, content_hash, completeness
             FROM run_gate_packets WHERE gate_instance_id = ?1",
                params![gate_instance_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;
        if let Some((_, bound_revision_id, _, bound_hash, completeness)) = &packet_binding {
            anyhow::ensure!(
                d.packet_revision_id.as_deref() == Some(bound_revision_id.as_str())
                    && d.packet_content_hash.as_deref() == Some(bound_hash.as_str()),
                "Gate conflict: submitted packet revision/hash is stale or missing; review the latest packet revision"
            );
            if d.approved {
                anyhow::ensure!(
                    completeness != "contradictory",
                    "Packet approval blocked: the bound revision has unresolved contradictions; request changes first"
                );
                anyhow::ensure!(
                    completeness != "incomplete" || d.acknowledge_incomplete,
                    "Packet approval blocked: incomplete packet requires explicit acknowledgement of open unknowns/decisions"
                );
            }
        }

        // A planning gate is bound to one exact plan revision and its source
        // packet. Any stale edit to either identity conflicts before writes.
        let plan_binding: Option<(String, String, i64, String, String, String)> = tx.query_row(
            "SELECT plan_id, revision_id, revision, content_hash, packet_revision_id, packet_content_hash
             FROM run_gate_plans WHERE gate_instance_id = ?1",
            params![gate_instance_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
        ).optional()?;
        if let Some((
            _,
            bound_revision_id,
            _,
            bound_hash,
            source_packet_revision,
            source_packet_hash,
        )) = &plan_binding
        {
            anyhow::ensure!(
                d.plan_revision_id.as_deref() == Some(bound_revision_id.as_str())
                    && d.plan_content_hash.as_deref() == Some(bound_hash.as_str()),
                "Gate conflict: submitted plan revision/hash is stale or missing; review the latest plan revision"
            );
            if let Some((_, packet_revision_id, _, packet_hash, _)) = &packet_binding {
                anyhow::ensure!(
                    packet_revision_id == source_packet_revision
                        && packet_hash == source_packet_hash,
                    "Gate conflict: the bound plan was generated from a different packet revision than this gate"
                );
            } else {
                // A plan-bound gate has no separate packet binding, but the
                // submitted source packet identity (used by the UI) must match
                // the exact packet the approved plan was generated from.
                match (
                    d.packet_revision_id.as_deref(),
                    d.packet_content_hash.as_deref(),
                ) {
                    (None, None) => {}
                    (Some(revision), Some(hash)) => anyhow::ensure!(
                        revision == source_packet_revision && hash == source_packet_hash,
                        "Gate conflict: submitted packet revision/hash is not the approved plan's source packet"
                    ),
                    _ => anyhow::bail!(
                        "Gate conflict: packet revision and content hash must be supplied together or omitted"
                    ),
                }
            }
        } else {
            anyhow::ensure!(
                d.plan_revision_id.is_none() && d.plan_content_hash.is_none(),
                "Gate conflict: this gate is not bound to a structured plan"
            );
            if packet_binding.is_none() {
                anyhow::ensure!(
                    d.packet_revision_id.is_none() && d.packet_content_hash.is_none(),
                    "Gate conflict: this gate is not bound to a requirements packet"
                );
            }
        }

        // Generic gates bind exactly one immutable output artifact. The exact
        // ID/hash the reviewer saw is required; stale or invented identity
        // conflicts before any review row or run transition is written.
        let output_binding: Option<(String, String, i64, String, String)> = tx
            .query_row(
                "SELECT flow_name, stage_name, artifact_version, content_hash, artifact_id
                 FROM run_gate_outputs WHERE gate_instance_id = ?1",
                params![gate_instance_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;
        if let Some((_, bound_stage, _, bound_hash, bound_artifact_id)) = &output_binding {
            anyhow::ensure!(
                packet_binding.is_none() && plan_binding.is_none(),
                "Gate conflict: generic output gate is also bound to a packet or plan"
            );
            anyhow::ensure!(
                d.output_artifact_id.as_deref() == Some(bound_artifact_id.as_str())
                    && d.output_content_hash.as_deref() == Some(bound_hash.as_str())
                    && d.output_stage_name.as_deref() == Some(bound_stage.as_str()),
                "Gate conflict: submitted output artifact/hash is stale or missing; review the latest output"
            );
        } else {
            anyhow::ensure!(
                d.output_artifact_id.is_none()
                    && d.output_content_hash.is_none()
                    && d.output_stage_name.is_none(),
                "Gate conflict: this gate is not bound to a generic output artifact"
            );
        }

        // Count durable accepted rejections only: stale/duplicate submissions
        // never reach this row, and this decision is the one being accepted now.
        let prior_rejections: i64 = tx.query_row(
            "SELECT COUNT(*) FROM gate_decisions WHERE run_id=?1 AND approved=0 AND disposition IN ('rework','rework_limit_cancelled')",
            params![d.run_id.as_str()], |row| row.get(0),
        )?;
        let rejection_number = usize::try_from(prior_rejections)
            .unwrap_or(usize::MAX)
            .saturating_add(1);
        let disposition = if d.approved {
            "approved"
        } else if rejection_number >= rework_limit {
            "rework_limit_cancelled"
        } else {
            "rework"
        };
        let status = if disposition == "rework_limit_cancelled" {
            RunStatus::Cancelled
        } else {
            RunStatus::Running
        };
        let revision = lifecycle::Revision::loaded(expected_state_version);
        lifecycle::guard_write(
            &tx,
            "pipeline_runs",
            d.run_id.as_str(),
            &revision,
            &status.to_string(),
        )?;
        let changed = tx.execute(
            "UPDATE pipeline_runs SET status=?1, gate_state=NULL, gate_instance_id=NULL, gate_name=NULL,
                 updated_at=?2 WHERE id=?3 AND revision=?4 AND status='waiting_for_gate' AND gate_instance_id=?5",
            params![status.to_string(), chrono::Utc::now().to_rfc3339(), d.run_id.as_str(), expected_state_version + 1, gate_instance_id],
        )?;
        anyhow::ensure!(
            changed == 1,
            "Gate state conflict for run {}",
            d.run_id.as_str()
        );

        let state_version = expected_state_version + 1;
        let decision_id = format!("gd_{}", uuid::Uuid::new_v4());
        let created_at = chrono::Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO gate_decisions (id, run_id, gate_name, actor_id, actor_name, approved, reason, note, created_at,
                 gate_instance_id, idempotency_key, expected_state_version, state_version, disposition,
                 packet_revision_id, packet_content_hash, plan_revision_id, plan_content_hash,
                 output_artifact_id, output_content_hash, output_stage_name, acknowledge_incomplete)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
            params![decision_id, d.run_id.as_str(), gate_name, d.actor_id, d.actor_name, d.approved as i64, d.reason, d.note,
                created_at, gate_instance_id, idempotency_key, expected_state_version, state_version, disposition,
                d.packet_revision_id, d.packet_content_hash, d.plan_revision_id, d.plan_content_hash,
                d.output_artifact_id, d.output_content_hash, d.output_stage_name,
                d.acknowledge_incomplete as i64],
        )?;
        if plan_binding.is_some() {
            // A planning gate reviews the plan; the packet was approved at the
            // Discovery gate. The plan review records both exact identities.
        } else if let Some((packet_id, revision_id, revision_number, content_hash, completeness)) =
            &packet_binding
        {
            packets::insert_packet_review_tx(
                &tx,
                d,
                &decision_id,
                &packets::GatePacketBinding {
                    gate_instance_id: gate_instance_id.to_string(),
                    run_id: d.run_id.as_str().to_string(),
                    packet_id: packet_id.clone(),
                    revision_id: revision_id.clone(),
                    revision: *revision_number,
                    content_hash: content_hash.clone(),
                    completeness: completeness.clone(),
                    bound_at: String::new(),
                    updated_at: String::new(),
                },
                &format!("rev_{}", uuid::Uuid::new_v4()),
                &created_at,
            )?;
        }
        if let Some((
            plan_id,
            revision_id,
            revision_number,
            content_hash,
            packet_revision_id,
            packet_content_hash,
        )) = &plan_binding
        {
            plans::insert_plan_review_tx(
                &tx,
                d,
                &decision_id,
                &plans::GatePlanBinding {
                    gate_instance_id: gate_instance_id.to_string(),
                    run_id: d.run_id.as_str().to_string(),
                    plan_id: plan_id.clone(),
                    revision_id: revision_id.clone(),
                    revision: *revision_number,
                    content_hash: content_hash.clone(),
                    packet_revision_id: packet_revision_id.clone(),
                    packet_content_hash: packet_content_hash.clone(),
                    bound_at: String::new(),
                    updated_at: String::new(),
                },
                &format!("prev_{}", uuid::Uuid::new_v4()),
                &created_at,
            )?;
        }
        tx.execute(
            "INSERT INTO events (id, run_id, kind, actor, flow_name, summary, payload, created_at) VALUES (?1, ?2, 'gate_decision', ?3, ?4, ?5, ?6, ?7)",
            params![format!("evt_{}", uuid::Uuid::new_v4()), d.run_id.as_str(), d.actor_id, gate_name,
                if d.approved { "approved" } else { "rejected" },
                serde_json::json!({"decision_id":decision_id,"gate_instance_id":gate_instance_id,"approved":d.approved,"reason":d.reason,
                    "disposition":disposition,"expected_state_version":expected_state_version,"state_version":state_version,
                    "output_artifact_id":d.output_artifact_id,"output_content_hash":d.output_content_hash,
                    "output_stage_name":d.output_stage_name}).to_string(), created_at],
        )?;
        lifecycle::finish_write(
            &tx,
            "pipeline_runs",
            d.run_id.as_str(),
            d.run_id.as_str(),
            &status.to_string(),
            &revision,
        )?;
        // Normalize the durable review identity kind for API/UI/receipts. This
        // does not replace or rewrite any of the exact identity fields.
        let gate_binding_kind = if plan_binding.is_some() {
            Some("plan".to_string())
        } else if packet_binding.is_some() {
            Some("packet".to_string())
        } else if output_binding.is_some() {
            Some("artifact".to_string())
        } else {
            None
        };
        // Record the follow-on route in the same transaction as the accepted
        // decision. A crash before the manager routes is then recoverable
        // without replaying model/tool/publication work.
        let mut receipt = GateDecisionReceipt {
            id: decision_id.clone(),
            run_id: d.run_id.as_str().to_string(),
            gate_instance_id: gate_instance_id.to_string(),
            approved: d.approved,
            status: status.clone(),
            state_version,
            disposition: disposition.into(),
            replayed: false,
            packet_revision_id: d.packet_revision_id.clone(),
            packet_content_hash: d.packet_content_hash.clone(),
            plan_revision_id: d.plan_revision_id.clone(),
            plan_content_hash: d.plan_content_hash.clone(),
            output_artifact_id: d.output_artifact_id.clone(),
            output_content_hash: d.output_content_hash.clone(),
            output_stage_name: d.output_stage_name.clone(),
            gate_binding_kind,
            effect_id: None,
        };
        if status != RunStatus::Cancelled {
            let effect_id = effects::insert_tx(
                &tx,
                d.run_id.as_str(),
                "gate_route",
                &format!("gate_route:{decision_id}"),
                &serde_json::json!({"decision": d, "receipt": &receipt}),
            )?;
            receipt.effect_id = Some(effect_id);
        }
        tx.commit()?;
        Ok(receipt)
    }

    pub fn list_gate_decisions(&self, run_id: &str) -> Result<Vec<GateDecision>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(
            "SELECT id, run_id, gate_name, actor_id, actor_name, approved, reason, note, created_at,
                    gate_instance_id, idempotency_key, expected_state_version, state_version, disposition,
                    packet_revision_id, packet_content_hash, plan_revision_id, plan_content_hash,
                    output_artifact_id, output_content_hash, output_stage_name,
                    acknowledge_incomplete
             FROM gate_decisions WHERE run_id = ?1 ORDER BY created_at, rowid",
        )?;
        let rows = stmt.query_map(params![run_id], |row| {
            Ok(GateDecision {
                id: row.get(0)?,
                run_id: RunId(row.get(1)?),
                gate_name: row.get(2)?,
                actor_id: row.get(3)?,
                actor_name: row.get(4)?,
                approved: row.get::<_, i64>(5)? != 0,
                reason: row.get(6)?,
                note: row.get(7)?,
                created_at: row.get(8)?,
                gate_instance_id: row.get(9)?,
                idempotency_key: row.get(10)?,
                expected_state_version: row.get(11)?,
                state_version: row.get(12)?,
                disposition: row.get(13)?,
                packet_revision_id: row.get(14)?,
                packet_content_hash: row.get(15)?,
                plan_revision_id: row.get(16)?,
                plan_content_hash: row.get(17)?,
                output_artifact_id: row.get(18)?,
                output_content_hash: row.get(19)?,
                output_stage_name: row.get(20)?,
                acknowledge_incomplete: row.get::<_, i64>(21)? != 0,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn append_stage_log(&self, l: &StageLog) -> Result<i64> {
        let conn = lock_conn!(self);
        // TJ-024: durable activity is inspectable by authorized members, so
        // credentials are redacted at the write boundary, not only in the UI.
        let content = redaction::redact_sensitive(&l.content);
        conn.execute(
            "INSERT INTO stage_logs (run_id, stage_id, stage_name, flow_name, kind, content, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![l.run_id.as_str(), l.stage_id, l.stage_name, l.flow_name, l.kind, content, l.created_at],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Stage activity rows with id > after_seq, oldest first.
    pub fn list_stage_logs(
        &self,
        run_id: &str,
        after_seq: i64,
        limit: i64,
    ) -> Result<Vec<StageLog>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(
            "SELECT id, run_id, stage_id, stage_name, flow_name, kind, content, created_at FROM stage_logs WHERE run_id = ?1 AND id > ?2 ORDER BY id LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![run_id, after_seq, limit], |row| {
            Ok(StageLog {
                id: row.get(0)?,
                run_id: RunId(row.get(1)?),
                stage_id: row.get(2)?,
                stage_name: row.get(3)?,
                flow_name: row.get(4)?,
                kind: row.get(5)?,
                content: row.get(6)?,
                created_at: row.get(7)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Bounded durable activity for diagnostics that are not run-scoped.
    pub fn list_recent_stage_logs(&self, limit: i64) -> Result<Vec<StageLog>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(
            "SELECT id, run_id, stage_id, stage_name, flow_name, kind, content, created_at
             FROM stage_logs ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit.clamp(1, 5000)], |row| {
            Ok(StageLog {
                id: row.get(0)?,
                run_id: RunId(row.get(1)?),
                stage_id: row.get(2)?,
                stage_name: row.get(3)?,
                flow_name: row.get(4)?,
                kind: row.get(5)?,
                content: row.get(6)?,
                created_at: row.get(7)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        out.reverse();
        Ok(out)
    }

    /// Durable tool/activity rows for one flow-stage lineage. Used by the
    /// Engineering publication gate to verify that claimed test commands were
    /// actually executed by the shell tool, not merely asserted by the model.
    pub fn list_stage_logs_for_flow_stage(
        &self,
        run_id: &str,
        flow_name: &str,
        stage_name: &str,
    ) -> Result<Vec<StageLog>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(
            "SELECT id, run_id, stage_id, stage_name, flow_name, kind, content, created_at
             FROM stage_logs
             WHERE run_id = ?1 AND flow_name = ?2 AND stage_name = ?3
             ORDER BY id",
        )?;
        let rows = stmt.query_map(params![run_id, flow_name, stage_name], |row| {
            Ok(StageLog {
                id: row.get(0)?,
                run_id: RunId(row.get(1)?),
                stage_id: row.get(2)?,
                stage_name: row.get(3)?,
                flow_name: row.get(4)?,
                kind: row.get(5)?,
                content: row.get(6)?,
                created_at: row.get(7)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn list_stage_logs_for_stage(&self, stage_id: &str, limit: i64) -> Result<Vec<StageLog>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(
            "SELECT id, run_id, stage_id, stage_name, flow_name, kind, content, created_at
             FROM stage_logs WHERE stage_id = ?1 ORDER BY id LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![stage_id, limit.clamp(1, 5000)], |row| {
            Ok(StageLog {
                id: row.get(0)?,
                run_id: RunId(row.get(1)?),
                stage_id: row.get(2)?,
                stage_name: row.get(3)?,
                flow_name: row.get(4)?,
                kind: row.get(5)?,
                content: row.get(6)?,
                created_at: row.get(7)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// True when the run has at least one stage currently waiting for input.
    pub fn run_has_waiting_stage(&self, run_id: &str) -> Result<bool> {
        let conn = lock_conn!(self);
        let n: i64 = conn.query_row(
            "SELECT COUNT(1) FROM stages WHERE status = 'waiting_for_input' AND flow_id IN (SELECT id FROM flows WHERE run_id = ?1)",
            params![run_id],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    pub fn append_event(&self, e: &RunEvent) -> Result<()> {
        let conn = lock_conn!(self);
        conn.execute(
            "INSERT INTO events (id, run_id, kind, actor, flow_name, stage_name, summary, payload, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![e.id, e.run_id.as_str(), e.kind, e.actor, e.flow_name, e.stage_name, e.summary, e.payload, e.created_at],
        )?;
        Ok(())
    }

    pub fn list_events(&self, run_id: &str) -> Result<Vec<RunEvent>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(
            "SELECT id, run_id, kind, actor, flow_name, stage_name, summary, payload, created_at FROM events WHERE run_id = ?1 ORDER BY created_at, rowid",
        )?;
        let rows = stmt.query_map(params![run_id], |row| {
            Ok(RunEvent {
                id: row.get(0)?,
                run_id: RunId(row.get(1)?),
                kind: row.get(2)?,
                actor: row.get(3)?,
                flow_name: row.get(4)?,
                stage_name: row.get(5)?,
                summary: row.get(6)?,
                payload: row.get(7)?,
                created_at: row.get(8)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn append_artifact(&self, a: &RunArtifact) -> Result<()> {
        let conn = lock_conn!(self);
        conn.execute(
            "INSERT INTO artifacts (id, run_id, flow_name, stage_name, attempt, version, content, content_hash, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![a.id, a.run_id.as_str(), a.flow_name, a.stage_name, a.attempt, a.version, a.content, a.content_hash, a.created_at],
        )?;
        Ok(())
    }

    pub fn append_next_artifact(&self, a: &mut RunArtifact) -> Result<()> {
        let conn = lock_conn!(self);
        let version: i64 = conn.query_row(
            "SELECT COALESCE(MAX(version), 0) + 1 FROM artifacts WHERE run_id = ?1 AND flow_name = ?2 AND stage_name = ?3 AND attempt = ?4",
            params![a.run_id.as_str(), a.flow_name, a.stage_name, a.attempt],
            |row| row.get(0),
        )?;
        a.version = version;
        conn.execute(
            "INSERT INTO artifacts (id, run_id, flow_name, stage_name, attempt, version, content, content_hash, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![a.id, a.run_id.as_str(), a.flow_name, a.stage_name, a.attempt, a.version, a.content, a.content_hash, a.created_at],
        )?;
        Ok(())
    }

    pub fn next_artifact_version(
        &self,
        run_id: &str,
        flow_name: &str,
        stage_name: &str,
        attempt: i64,
    ) -> Result<i64> {
        let conn = lock_conn!(self);
        let v: i64 = conn.query_row(
            "SELECT COALESCE(MAX(version), 0) + 1 FROM artifacts WHERE run_id = ?1 AND flow_name = ?2 AND stage_name = ?3 AND attempt = ?4",
            params![run_id, flow_name, stage_name, attempt],
            |row| row.get(0),
        )?;
        Ok(v)
    }

    pub fn list_artifacts(&self, run_id: &str) -> Result<Vec<RunArtifact>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(
            "SELECT id, run_id, flow_name, stage_name, attempt, version, content, content_hash, created_at FROM artifacts WHERE run_id = ?1 ORDER BY flow_name, stage_name, version",
        )?;
        let rows = stmt.query_map(params![run_id], |row| {
            Ok(RunArtifact {
                id: row.get(0)?,
                run_id: RunId(row.get(1)?),
                flow_name: row.get(2)?,
                stage_name: row.get(3)?,
                attempt: row.get(4)?,
                version: row.get(5)?,
                content: row.get(6)?,
                content_hash: row.get(7)?,
                created_at: row.get(8)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Number of distinct stages of one flow that produced artifacts
    /// (versions/attempts collapse so retries don't inflate the count).
    pub fn count_artifact_stages(&self, run_id: &str, flow_name: &str) -> Result<usize> {
        let conn = lock_conn!(self);
        let n: i64 = conn.query_row(
            "SELECT COUNT(DISTINCT stage_name) FROM artifacts WHERE run_id = ?1 AND flow_name = ?2",
            params![run_id, flow_name],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    /// Timestamp of the run's latest observable activity (newest of last event,
    /// last stage completion/start/creation). None when the run has no rows.
    pub fn last_run_activity(&self, run_id: &str) -> Result<Option<String>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(
            "SELECT MAX(ts) FROM (
                SELECT MAX(created_at) AS ts FROM events WHERE run_id = ?1
                UNION ALL
                SELECT MAX(coalesce(completed_at, started_at, created_at)) AS ts FROM stages
                    WHERE flow_id IN (SELECT id FROM flows WHERE run_id = ?1)
            )",
        )?;
        let ts = stmt.query_row(params![run_id], |r| r.get::<_, Option<String>>(0))?;
        Ok(ts)
    }

    pub fn fork_artifact(
        &self,
        source_id: &str,
        new_id: &str,
        forked_at: &str,
    ) -> Result<RunArtifact> {
        let conn = lock_conn!(self);
        let src = conn.query_row(
            "SELECT run_id, flow_name, stage_name, attempt, version, content, content_hash, created_at FROM artifacts WHERE id = ?1",
            params![source_id],
            |row| {
                Ok(RunArtifact {
                    id: String::new(),
                    run_id: RunId(row.get(0)?),
                    flow_name: row.get(1)?,
                    stage_name: row.get(2)?,
                    attempt: row.get(3)?,
                    version: row.get(4)?,
                    content: row.get(5)?,
                    content_hash: row.get(6)?,
                    created_at: row.get(7)?,
                })
            },
        ).map_err(|e| anyhow::anyhow!("artifact '{}' not found: {}", source_id, e))?;
        // Compute next version for (run, flow, stage, attempt)
        let next_version: i64 = conn.query_row(
            "SELECT COALESCE(MAX(version), 0) + 1 FROM artifacts WHERE run_id = ?1 AND flow_name = ?2 AND stage_name = ?3 AND attempt = ?4",
            params![src.run_id.as_str(), src.flow_name, src.stage_name, src.attempt],
            |row| row.get(0),
        )?;
        let fork = RunArtifact {
            id: new_id.to_string(),
            run_id: src.run_id.clone(),
            flow_name: src.flow_name,
            stage_name: src.stage_name,
            attempt: src.attempt,
            version: next_version,
            content: src.content,
            content_hash: src.content_hash,
            created_at: forked_at.to_string(),
        };
        conn.execute(
            "INSERT INTO artifacts (id, run_id, flow_name, stage_name, attempt, version, content, content_hash, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![fork.id, fork.run_id.as_str(), fork.flow_name, fork.stage_name, fork.attempt, fork.version, fork.content, fork.content_hash, fork.created_at],
        )?;
        Ok(fork)
    }
}

#[cfg(test)]
mod gate_decision_tests;

#[cfg(test)]
mod auth_token_tests {
    use super::*;

    fn test_db() -> Database {
        let db = Database {
            conn: Mutex::new(rusqlite::Connection::open_in_memory().unwrap()),
        };
        db.migrate().unwrap();
        db.create_user(&User {
            id: "user".into(),
            username: "test-user".into(),
            password_hash: "unused".into(),
            display_name: None,
            is_admin: false,
            created_at: String::new(),
        })
        .unwrap();
        db
    }

    fn now() -> chrono::DateTime<chrono::Utc> {
        "2026-10-10T12:00:00Z".parse().unwrap()
    }

    fn issue_at(db: &Database, token: &str, issued_at: &str) {
        db.issue_token(token, "user").unwrap();
        lock_conn!(db)
            .execute(
                "UPDATE auth_tokens SET created_at = ?2, last_used_at = ?2 WHERE token = ?1",
                params![token, issued_at],
            )
            .unwrap();
    }

    #[test]
    fn new_token_works_for_an_old_account() {
        let db = test_db();
        let account_created = (chrono::Utc::now() - chrono::Duration::days(31)).to_rfc3339();
        lock_conn!(db)
            .execute("UPDATE users SET created_at = ?1", params![account_created])
            .unwrap();
        db.issue_token("new", "user").unwrap();
        let user = db.user_for_token("new").unwrap().unwrap();
        assert_eq!(user.id, "user");
        assert_eq!(user.created_at, account_created);
    }

    #[test]
    fn token_expires_at_exactly_thirty_days_independent_of_account_age() {
        let db = test_db();
        for (token, age, valid) in [
            (
                "before",
                chrono::Duration::days(30) - chrono::Duration::milliseconds(1),
                true,
            ),
            ("boundary", chrono::Duration::days(30), false),
            (
                "after",
                chrono::Duration::days(30) + chrono::Duration::milliseconds(1),
                false,
            ),
        ] {
            issue_at(&db, token, &(now() - age).to_rfc3339());
            assert_eq!(
                db.user_for_token_at(token, now()).unwrap().is_some(),
                valid,
                "{token}"
            );
            let remaining: i64 = lock_conn!(db)
                .query_row(
                    "SELECT COUNT(*) FROM auth_tokens WHERE token = ?1",
                    params![token],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(remaining, i64::from(valid));
        }
    }

    #[test]
    fn malformed_and_future_issue_times_are_rejected_and_removed() {
        let db = test_db();
        for issued in ["", "invalid", "2026-10-10T12:00:01Z"] {
            issue_at(&db, "invalid", issued);
            assert!(db.user_for_token_at("invalid", now()).unwrap().is_none());
            let remaining: i64 = lock_conn!(db)
                .query_row("SELECT COUNT(*) FROM auth_tokens", [], |r| r.get(0))
                .unwrap();
            assert_eq!(remaining, 0);
        }
    }

    #[test]
    fn missing_issue_time_is_rejected_and_removed() {
        let db = test_db();
        db.issue_token("missing", "user").unwrap();
        lock_conn!(db)
            .execute("UPDATE auth_tokens SET created_at = NULL", [])
            .unwrap();
        assert!(db.user_for_token_at("missing", now()).unwrap().is_none());
        let remaining: i64 = lock_conn!(db)
            .query_row("SELECT COUNT(*) FROM auth_tokens", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 0);
    }

    #[test]
    fn last_use_does_not_extend_absolute_expiry_and_offsets_are_supported() {
        let db = test_db();
        issue_at(&db, "active", "2026-09-10T14:00:00+02:00");
        let before = now() - chrono::Duration::milliseconds(1);
        assert!(db.user_for_token_at("active", before).unwrap().is_some());
        let (issued, used): (String, String) = lock_conn!(db)
            .query_row(
                "SELECT created_at, last_used_at FROM auth_tokens WHERE token = 'active'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(issued, "2026-09-10T14:00:00+02:00");
        assert_eq!(used, before.to_rfc3339());
        assert!(db.user_for_token_at("active", now()).unwrap().is_none());
    }

    #[test]
    fn unknown_and_revoked_tokens_stay_invalid() {
        let db = test_db();
        assert!(db.user_for_token_at("unknown", now()).unwrap().is_none());
        issue_at(&db, "revoked", &now().to_rfc3339());
        assert!(db.user_for_token_at("revoked", now()).unwrap().is_some());
        db.revoke_token("revoked").unwrap();
        assert!(db.user_for_token_at("revoked", now()).unwrap().is_none());
    }
}

#[cfg(test)]
mod artifact_tests {
    use super::*;

    fn test_db() -> Database {
        let dir = std::env::temp_dir().join(format!("tjdb-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Database::open(&dir.join("state.db").to_string_lossy()).expect("test db")
    }

    #[test]
    fn artifact_append_list_fork_roundtrip() {
        let db = test_db();
        let a = RunArtifact {
            id: "art_a".into(),
            run_id: RunId("run_x".into()),
            flow_name: "flow-a".into(),
            stage_name: "stage-a".into(),
            attempt: 0,
            version: 1,
            content: "{\"goal\":\"test\"}".into(),
            content_hash: "abc123".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
        };
        db.append_artifact(&a).expect("append");
        let listed = db.list_artifacts("run_x").expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].content, "{\"goal\":\"test\"}");

        let fork = db
            .fork_artifact("art_a", "art_fork", "2026-01-02T00:00:00Z")
            .expect("fork");
        assert_eq!(fork.version, 2);
        assert_eq!(fork.content, a.content);
        assert_eq!(fork.created_at, "2026-01-02T00:00:00Z");

        // original untouched (immutability), fork is a new row
        let listed = db.list_artifacts("run_x").expect("list2");
        assert_eq!(listed.len(), 2);
        let v1 = listed.iter().find(|x| x.id == "art_a").unwrap();
        assert_eq!(v1.version, 1);

        // next version computation
        let next = db
            .next_artifact_version("run_x", "flow-a", "stage-a", 0)
            .unwrap();
        assert_eq!(next, 3);
    }

    #[test]
    fn count_artifact_stages_collapses_versions() {
        let db = test_db();
        let mk = |id: &str, stage: &str, attempt: i64, version: i64| RunArtifact {
            id: id.into(),
            run_id: RunId("run_c".into()),
            flow_name: "flow-a".into(),
            stage_name: stage.into(),
            attempt,
            version,
            content: "{}".into(),
            content_hash: "h".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
        };
        db.append_artifact(&mk("c1", "stage-a", 0, 1)).unwrap();
        db.append_artifact(&mk("c2", "stage-a", 0, 2)).unwrap();
        db.append_artifact(&mk("c3", "stage-b", 1, 1)).unwrap();
        assert_eq!(db.count_artifact_stages("run_c", "flow-a").unwrap(), 2);
        assert_eq!(db.count_artifact_stages("run_c", "flow-b").unwrap(), 0);
    }

    #[test]
    fn artifact_version_unique_constraint() {
        let db = test_db();
        let mk = |v: i64| RunArtifact {
            id: format!("art_u{}", v),
            run_id: RunId("run_u".into()),
            flow_name: "flow-a".into(),
            stage_name: "stage-a".into(),
            attempt: 0,
            version: v,
            content: "{}".into(),
            content_hash: "h".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
        };
        db.append_artifact(&mk(1)).expect("first ok");
        // duplicate version on same lineage must be rejected by UNIQUE index
        assert!(db.append_artifact(&mk(1)).is_err());
        // same version on different lineage is fine
        let mut diff = mk(1);
        diff.id = "art_u_diff".into();
        diff.stage_name = "stage-b".into();
        db.append_artifact(&diff).expect("different stage ok");
    }

    #[test]
    fn fork_missing_artifact_errors() {
        let db = test_db();
        assert!(db.fork_artifact("art_nope", "art_x", "now").is_err());
    }

    #[test]
    fn events_append_ordering() {
        let db = test_db();
        for (i, kind) in ["run_started", "flow_started", "stage_started"]
            .iter()
            .enumerate()
        {
            db.append_event(&RunEvent {
                id: format!("evt_{}", i),
                run_id: RunId("run_e".into()),
                kind: kind.to_string(),
                actor: None,
                flow_name: Some("flow-a".into()),
                stage_name: None,
                summary: None,
                payload: None,
                created_at: "2026-01-01T00:00:0".to_string() + &i.to_string() + "Z",
            })
            .unwrap();
        }
        let evts = db.list_events("run_e").unwrap();
        assert_eq!(evts.len(), 3);
        assert_eq!(evts[0].kind, "run_started");
        assert_eq!(evts[2].kind, "stage_started");
    }
}

#[cfg(test)]
mod workflow_registry_tests {
    use super::*;

    fn test_db() -> Database {
        let dir = std::env::temp_dir().join(format!("tjdb-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Database::open(&dir.join("state.db").to_string_lossy()).expect("test db")
    }

    fn wf(name: &str, hash: &str) -> WorkflowVersion {
        WorkflowVersion {
            id: format!("wf_{}", uuid::Uuid::new_v4()),
            name: name.into(),
            smg_path: "fixture/synthetic-graph.json".into(),
            version: 1,
            revision: 0,
            description: None,
            content_hash: hash.into(),
            content_json: "{}".into(),
            entry: Some("flow-a".into()),
            exit: Some("flow-c".into()),
            flow_count: 6,
            gate_count: 2,
            stage_count: 18,
            catalog_id: None,
            catalog_label: None,
            catalog_description: None,
            catalog_default: false,
            capability_schema_version: 0,
            capability_hash: None,
            source_id: None,
            source_rel_path: None,
            source_kind: None,
            created_at: "2026-09-09T00:00:00Z".into(),
        }
    }

    #[test]
    fn register_dedupes_by_content_and_counts_revisions_per_name() {
        let db = test_db();
        let a = db.register_workflow(&wf("Pipeline", "hash-a")).unwrap();
        assert_eq!(a.revision, 1);

        // identical content → same row returned, no new revision
        let again = db.register_workflow(&wf("Pipeline", "hash-a")).unwrap();
        assert_eq!(again.id, a.id);
        assert_eq!(again.revision, 1);

        // new content, same name → revision 2; different name → its own revision 1
        let b = db.register_workflow(&wf("Pipeline", "hash-b")).unwrap();
        assert_eq!(b.revision, 2);
        let other = db.register_workflow(&wf("Other", "hash-c")).unwrap();
        assert_eq!(other.revision, 1);

        let list = db.list_workflows().unwrap();
        assert_eq!(list.len(), 3);
    }

    fn asset(logical: &str, content: &str) -> WorkflowAsset {
        WorkflowAsset {
            logical_path: logical.into(),
            content_hash: hex::encode(Sha256::digest(content.as_bytes())),
            content: content.as_bytes().to_vec(),
        }
    }

    #[test]
    fn package_assets_are_immutable_and_persisted_with_their_version() {
        let db = test_db();
        let assets = vec![
            asset("workflow.smg.json", "{\"version\":1}"),
            asset("agents/one/prompt.md", "package prompt"),
        ];
        let registered = db
            .register_workflow_with_assets(&wf("Package", "package-hash"), &assets)
            .unwrap();
        assert_eq!(db.workflow_asset_count(&registered.id).unwrap(), 2);
        let stored = db.workflow_assets(&registered.id).unwrap();
        assert_eq!(
            stored
                .iter()
                .map(|asset| asset.logical_path.as_str())
                .collect::<Vec<_>>(),
            vec!["agents/one/prompt.md", "workflow.smg.json"]
        );
        assert_eq!(stored[1].content, b"{\"version\":1}");

        // Identical assets are idempotent and keep the same version/assets.
        let again = db
            .register_workflow_with_assets(&wf("Package", "package-hash"), &assets)
            .unwrap();
        assert_eq!(again.id, registered.id);
        assert_eq!(db.workflow_assets(&registered.id).unwrap().len(), 2);

        // A changed prompt with unchanged graph content cannot mutate the
        // existing immutable version.
        let changed = vec![
            asset("workflow.smg.json", "{\"version\":1}"),
            asset("agents/one/prompt.md", "edited prompt"),
        ];
        let error = db
            .register_workflow_with_assets(&wf("Package", "package-hash"), &changed)
            .unwrap_err()
            .to_string();
        assert!(error.contains("different package assets"), "{error}");
        assert_eq!(db.list_workflows().unwrap().len(), 1);
        assert_eq!(
            db.workflow_assets(&registered.id).unwrap()[0].content,
            b"package prompt"
        );
    }

    #[test]
    fn duplicate_asset_logical_paths_fail_before_any_workflow_row() {
        let db = test_db();
        let assets = vec![
            asset("agents/one/agent.json", "{\"id\":\"one\"}"),
            asset("agents/one/agent.json", "{\"id\":\"two\"}"),
        ];
        let error = db
            .register_workflow_with_assets(&wf("Duplicate", "duplicate-hash"), &assets)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("duplicate package asset logical path"),
            "{error}"
        );
        assert!(db.list_workflows().unwrap().is_empty());
    }

    #[test]
    fn legacy_registration_does_not_create_asset_rows() {
        let db = test_db();
        let registered = db.register_workflow(&wf("Legacy", "legacy-hash")).unwrap();
        assert_eq!(db.workflow_asset_count(&registered.id).unwrap(), 0);
        assert!(db.workflow_assets(&registered.id).unwrap().is_empty());
    }

    #[test]
    fn workflow_assets_table_survives_migration_replay() {
        let dir = std::env::temp_dir().join(format!("tjdb-assets-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.db");
        let first = Database::open(&path.to_string_lossy()).unwrap();
        let registered = first
            .register_workflow_with_assets(
                &wf("Replay", "replay-hash"),
                &[asset("workflow.smg.json", "{\"version\":1}")],
            )
            .unwrap();
        drop(first);
        let reopened = Database::open(&path.to_string_lossy()).unwrap();
        assert_eq!(reopened.workflow_asset_count(&registered.id).unwrap(), 1);
    }

    #[test]
    fn lane_pin_roundtrip_and_workflow_id_migration() {
        let db = test_db();
        let wf1 = db.register_workflow(&wf("Pipeline", "hash-x")).unwrap();

        let project = Project {
            id: "proj_1".into(),
            name: "P".into(),
            repo_url: None,
            base_branch: "main".into(),
            workspace_path: None,
            git_provider: None,
            profile: "full".into(),
            workspace_kind: None,
            catalog_id: None,
            provisioning_state: None,
            provisioning_error: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        };
        db.save_project(&project).unwrap();
        let lane = Lane {
            id: "lane_1".into(),
            project_id: ProjectId("proj_1".into()),
            name: "Default".into(),
            smg_path: String::new(),
            description: None,
            workflow_id: None,
            created_at: "2026-01-01T00:00:00Z".into(),
        };
        db.save_lane(&lane).unwrap();

        // unpinned by default
        assert_eq!(db.load_lane("lane_1").unwrap().unwrap().workflow_id, None);

        // pin + read back
        db.set_lane_workflow("lane_1", Some(&wf1.id)).unwrap();
        assert_eq!(
            db.load_lane("lane_1")
                .unwrap()
                .unwrap()
                .workflow_id
                .as_deref(),
            Some(wf1.id.as_str())
        );
        assert_eq!(db.lanes_using_workflow(&wf1.id).unwrap().len(), 1);

        // unpin
        db.set_lane_workflow("lane_1", None).unwrap();
        assert_eq!(db.load_lane("lane_1").unwrap().unwrap().workflow_id, None);

        // pinning to unknown lane errors
        assert!(db.set_lane_workflow("lane_missing", Some(&wf1.id)).is_err());
    }

    #[test]
    fn runs_carry_workflow_id() {
        let db = test_db();
        let mut run = PipelineRun::new(
            "run_wf".into(),
            None,
            None,
            "desc".into(),
            RunStatus::Running,
            None,
            Some("main".into()),
            Some("trace".into()),
            None,
        );
        run.set_workflow_id(Some("wf_abc".into()));
        db.save_run(&run).unwrap();
        let loaded = db.load_run("run_wf").unwrap().unwrap();
        assert_eq!(loaded.workflow_id(), Some("wf_abc"));
    }

    #[test]
    fn execution_bundle_is_immutable_and_carried_on_the_run() {
        let db = test_db();
        let mut run = PipelineRun::new(
            "run_bundle".into(),
            None,
            None,
            "desc".into(),
            RunStatus::Running,
            None,
            Some("main".into()),
            Some("trace".into()),
            None,
        );
        let record = ExecutionBundleRecord {
            id: "bundle_1".into(),
            run_id: RunId("run_bundle".into()),
            workflow_id: Some("wf_1".into()),
            bundle_hash: "hash-one".into(),
            bundle_json: "{\"schema_version\":1}".into(),
            legacy: false,
            created_at: "2026-10-21T00:00:00Z".into(),
        };
        db.save_execution_bundle(&record).unwrap();
        run.set_execution_bundle(Some(record.id.clone()), Some(record.bundle_hash.clone()));
        db.save_run(&run).unwrap();

        let loaded = db.load_run("run_bundle").unwrap().unwrap();
        assert_eq!(loaded.execution_bundle_id(), Some("bundle_1"));
        assert_eq!(loaded.execution_bundle_hash(), Some("hash-one"));
        assert_eq!(
            db.execution_bundle_for_run("run_bundle")
                .unwrap()
                .unwrap()
                .bundle_json,
            record.bundle_json
        );

        // A second, different bundle for the same run cannot replace it.
        let mut changed = record.clone();
        changed.id = "bundle_2".into();
        changed.bundle_hash = "hash-two".into();
        assert!(db.save_execution_bundle(&changed).is_err());
        assert_eq!(
            db.execution_bundle_for_run("run_bundle")
                .unwrap()
                .unwrap()
                .id,
            "bundle_1"
        );
    }
}

#[cfg(test)]
mod stale_stage_tests {
    use super::*;

    fn test_db() -> Database {
        let dir = std::env::temp_dir().join(format!("tjdb-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Database::open(&dir.join("state.db").to_string_lossy()).expect("test db")
    }

    #[test]
    fn stale_waiting_stages_are_finalized() {
        let db = test_db();
        let mut run = PipelineRun::new(
            "run_stale".into(),
            None,
            None,
            "d".into(),
            RunStatus::Running,
            None,
            Some("main".into()),
            Some("t".into()),
            None,
        );
        db.save_run(&run).unwrap();
        let flow = FlowRecord::new(
            "flow_s".into(),
            RunId("run_stale".into()),
            "flow-a".into(),
            Some(1),
            None,
            None,
        );
        db.save_flow(&flow).unwrap();
        let mut stage = StageRecord::new(
            "stage_stale".into(),
            FlowId("flow_s".into()),
            "stage-b".into(),
            StageStatus::WaitingForInput,
            0,
            None,
        );
        db.save_stage(&stage).unwrap();

        run.set_status(RunStatus::WaitingForGate);
        db.save_run(&run).unwrap();
        let n = db
            .finalize_stale_waiting_stages("run_stale", "run moved on")
            .unwrap();
        assert_eq!(n, 1);
        stage = db.load_stage("stage_stale").unwrap().unwrap();
        assert_eq!(stage.status(), &StageStatus::Failed);
        assert_eq!(stage.error().unwrap(), "run moved on");

        // idempotent
        assert_eq!(
            db.finalize_stale_waiting_stages("run_stale", "again")
                .unwrap(),
            0
        );
    }
}
