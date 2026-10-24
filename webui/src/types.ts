export type RunStatus = 'running' | 'waiting_for_gate' | 'interrupted' | 'completed' | 'failed' | 'cancelled';
export type FlowStatus = 'pending' | 'running' | 'completed' | 'failed' | 'timed_out';
export type StageStatus = 'pending' | 'running' | 'waiting_for_input' | 'completed' | 'failed' | 'timed_out';
// Catalog ids are arbitrary strings from the registry; no closed profile union.
export type GateBindingKind = 'artifact' | 'packet' | 'plan' | 'none';

export interface Project { id: string; name: string; repo_url: string | null; base_branch: string; workspace_path: string | null; profile?: string; catalog_id?: string | null; workspace_kind?: 'none' | 'repository' | string | null; provisioning_state?: string | null; provisioning_error?: string | null; created_at: string; lanes: Lane[] }
export interface Lane { id: string; project_id: string; name: string; smg_path: string; description?: string | null; workflow_id?: string | null; created_at?: string }

export interface TjRun {
  id: string; project_id: string | null; lane_id: string | null; description: string;
  status: RunStatus; current_flow_id: string | null; gate_state: string | null;
  gate_instance_id: string | null; gate_name: string | null; state_version: number;
  repo_url: string | null; base_branch: string | null; slug: string | null;
  pr_url: string | null; pr_number: number | null; created_at: string; updated_at: string;
  execution_bundle_id?: string | null; execution_bundle_hash?: string | null;
  workflow_id?: string | null;
  catalog_id?: string | null;
  profile?: string;
  /** Normalized durable review identity for the active gate. `kind` is the only
   * field panels should branch on; `profile` remains legacy display metadata. */
  gate_binding?: GateRunBinding | null;
}

export interface GateRunBinding {
  kind: GateBindingKind; gate_instance_id: string;
  flow_name: string | null; stage_name: string | null;
  artifact_id: string | null; artifact_version: number | null; content_hash: string | null;
  packet_revision_id: string | null; packet_content_hash: string | null;
  plan_revision_id: string | null; plan_content_hash: string | null;
}

export interface TjStage {
  id: string; stage_name: string; status: StageStatus; iteration: number;
  error: string | null; started_at: string | null; completed_at: string | null;
  input_artifacts: unknown; output_artifacts: unknown;
}

export interface TjFlow {
  id: string; flow_name: string; flow_number: number | null; status: FlowStatus;
  branch_name: string | null; started_at: string | null; completed_at: string | null;
  stages: TjStage[];
}

export interface RunTree { run: TjRun; flows: TjFlow[] }

export interface GateDecision {
  id: string; run_id: string; gate_name: string; actor_id: string; actor_name: string;
  approved: boolean; reason: string | null; note: string | null; created_at: string;
  gate_instance_id?: string | null; idempotency_key?: string | null;
  expected_state_version?: number | null; state_version?: number | null; disposition?: string | null;
  packet_revision_id?: string | null; packet_content_hash?: string | null;
  plan_revision_id?: string | null; plan_content_hash?: string | null;
  output_artifact_id?: string | null; output_content_hash?: string | null; output_stage_name?: string | null;
}

export interface GateDecisionReceipt {
  id: string; run_id: string; gate_instance_id: string; approved: boolean;
  status: RunStatus; state_version: number;
  disposition: 'approved' | 'rework' | 'rework_limit_cancelled'; replayed: boolean;
  packet_revision_id?: string | null; packet_content_hash?: string | null;
  plan_revision_id?: string | null; plan_content_hash?: string | null;
  output_artifact_id?: string | null; output_content_hash?: string | null; output_stage_name?: string | null;
  gate_binding_kind?: Exclude<GateBindingKind, 'none'> | null;
}

export interface WorkflowVersion {
  id: string; name: string; smg_path: string; source_id?: string | null; source_rel_path?: string | null;
  version: number; revision: number;
  description: string | null; maturity?: 'supported' | 'experimental'; entry: string | null; exit: string | null;
  catalog_id?: string | null; catalog_label?: string | null; catalog_description?: string | null;
  catalog_default?: boolean; capability_schema_version?: number; capability_hash?: string | null;
  flow_count: number; gate_count: number; stage_count: number; created_at: string;
  content: {
    name?: string; description?: string; entry?: string; exit?: string;
    'flow-order'?: string[];
    flows?: Record<string, { type?: 'flow' | 'gate'; entry?: string; exit?: string; routes?: Record<string, unknown>; stages?: Record<string, { agent?: string; interactive?: boolean; routes?: Record<string, unknown> }> }>;
  };
  lanes: { id: string; project_id: string; name: string }[];
}

export interface CatalogVersion { workflow_id: string; revision: number; content_hash: string }
export interface WorkflowProfile {
  id: string; catalog_id: string; label: string; description: string;
  workflow_id: string; smg_path: string; maturity: 'supported' | 'experimental';
  repository_free: boolean; gate_binding: GateBindingKind; default: boolean;
  versions: CatalogVersion[];
}
export interface WorkflowConfigured { sources: { id: string; path: string; kind: 'file' | 'directory' }[] }

export interface User { id: string; username: string; display_name: string | null; is_admin: boolean }
export interface AuthResponse { token: string; user: User }

export interface TjArtifact {
  id: string; run_id: string; flow_name: string; stage_name: string;
  attempt: number; version: number; content: string; content_hash: string; created_at: string;
}

export interface TjEvent {
  id: string; run_id: string; kind: string; actor: string | null;
  flow_name: string | null; stage_name: string | null;
  summary: string | null; payload: string | null; created_at: string;
}

export interface StageLog {
  id: number; run_id: string; stage_id: string; stage_name: string | null;
  flow_name: string | null;
  kind:  string;
  content: string; created_at: string;
}

export interface ChatMsg { who: 'you' | 'chief'; text: string }

/// Mirror of tj-runner's QuestionOption (types.rs; plan T-2) — single source of truth.
export interface QuestionOption { id: string; label: string }

/// Mirror of tj-runner's QuestionRequest — the structured payload of the
/// `question` tool, carried verbatim in `question` stage-log rows (plan T-5).
export interface QuestionRequest {
  question: string;
  options: QuestionOption[];
  multi_select: boolean;
  free_form: boolean;
}

/// Mirror of tj-runner's QuestionAnswer. `selected` carries option ids;
/// `text` is the free-form reply. Serialized as the POST /input `text` field
/// (OQ-2) and parsed back by tj-stage's question channel.
export interface QuestionAnswer {
  selected: string[];
  text?: string | null;
}

/// Durable question identity returned by GET /runs/{id}/questions and embedded
/// in the `question` stage-log payload (TJ-014).
export interface StageQuestion {
  id: string;
  run_id: string;
  stage_id: string;
  stage_name: string;
  flow_name: string;
  attempt: number;
  status: 'open' | 'answered' | 'closed' | 'cancelled';
  request: QuestionRequest;
  answer?: QuestionAnswer | null;
  answer_source?: string | null;
}

export interface QuestionAnswerInput {
  stageId: string;
  attempt: number;
  questionId: string;
  idempotencyKey?: string;
}

export const REJECT_REASONS = [
  'Scope too broad — needs splitting',
  'Wrong owner assigned',
  'Missing acceptance criteria',
  'Touches data it should not',
  'Conflicts with a policy page',
  'Other',
] as const;

/** TJ-016 durable run budget snapshot. Values are authoritative counters. */
export interface RunBudget {
  run_id: string;
  question_rounds_used: number; question_rounds_limit: number; question_rounds_remaining: number;
  question_rounds_per_stage_limit: number;
  model_calls_used: number; model_calls_limit: number; model_calls_remaining: number;
  tool_calls_used: number; tool_calls_limit: number; tool_calls_remaining: number;
  output_corrections_used: number; output_corrections_limit: number; output_corrections_remaining: number;
  interaction_nudges_used: number; interaction_nudges_limit: number; interaction_nudges_remaining: number;
  active_ms_used: number; active_ms_limit: number; active_ms_remaining: number;
  cost_micros_used: number; cost_micros_limit: number; cost_micros_remaining: number;
  unknown_usage_calls: number;
  exhausted: boolean; exhausted_reason: string | null; updated_at: string;
}

export interface ReadinessCheck {
  name: string; ok: boolean; detail?: string | null;
  schema_version?: number | null;
  provider_id?: string | null; model?: string | null; agent_id?: string | null;
  credential_env?: string | null; path?: string | null; count?: number | null;
}

export interface Readiness {
  status: 'ready' | 'degraded' | 'blocked';
  version: string;
  checks: ReadinessCheck[];
  first_run: {
    admin_exists: boolean; project_count: number; setup_required: boolean; next_step: string;
  };
  provider: {
    agent_id: string | null; provider_id: string | null; model: string | null;
    credential_env: string | null; ready: boolean;
  };
  activity_retention_days: number;
}

export interface ProviderUsageSummary {
  calls: number; known_usage_calls: number; unknown_usage_calls: number;
  input_tokens: number; output_tokens: number; cost_micros: number;
}

export interface ProviderAttemptSummary {
  total: number; succeeded: number; rate_limited: number; transient: number;
  credential: number; permanent: number;
}

export interface RunDiagnostics {
  run_id: string;
  status: string;
  state_version: number;
  gate_instance_id: string | null;
  gate_name: string | null;
  generated_at: string;
  retention_days: number;
  stages: { flow_name: string; stage_id: string; stage_name: string; status: string; attempt_iteration: number; error: string | null }[];
  routes: { gate_name: string; approved: boolean; disposition: string | null; packet_revision_id: string | null; packet_content_hash: string | null; created_at: string }[];
  corrections: { llm_calls: number; output_corrections: number; silent_turn_nudges: number };
  provider_usage: ProviderUsageSummary;
  provider_attempts: ProviderAttemptSummary;
  budget: RunBudget | null;
  recovery: RecoveryState | null;
}

export interface PendingEffect {
  id: string; run_id: string; kind: string;
  status: 'pending' | 'running' | 'completed' | 'ambiguous' | 'failed';
  idempotency_key: string; payload: unknown; created_at: string; updated_at: string;
  completed_at: string | null; error: string | null;
}

export interface RecoveryState {
  run_id: string; interrupted_at: string; reason: string;
  resume_flow: string | null; resume_stage: string | null;
  effects_unknown: boolean; retry_count: number; updated_at: string;
}

export interface RetryResult {
  accepted: boolean;
  plan: { run_id: string; flow_name: string; attempt_offset: number; effects_unknown_acknowledged: boolean };
}


/** TJ-020/TJ-022 requirements packet v1. Sections are optional in review
 * payloads for forward-compatible rendering, but server validation enforces
 * the actual schema before persistence. */
export interface PacketContent {
  schema_version?: number;
  title?: string;
  problem?: { statement?: string; source_refs?: string[] };
  requirements?: Record<string, unknown>[];
  evidence?: Record<string, unknown>[];
  unknowns?: Record<string, unknown>[];
  decisions?: Record<string, unknown>[];
  contradictions?: Record<string, unknown>[];
  sources?: { id: string; kind: string; reference: string; excerpt: string }[];
  [key: string]: unknown;
}

export interface PacketRevision {
  id: string;
  packet_id: string;
  run_id: string;
  revision: number;
  schema_version: number;
  content_hash: string;
  completeness: 'complete' | 'incomplete' | 'contradictory';
  parent_revision_id: string | null;
  source_artifact_id: string | null;
  source_artifact_hash: string | null;
  flow_name: string | null;
  stage_name: string | null;
  change_request: string | null;
  created_by: string;
  created_at: string;
  content: PacketContent;
  replayed?: boolean;
}

export interface PacketReview {
  id: string;
  revision_id: string;
  revision: number;
  content_hash: string;
  gate_instance_id: string;
  actor_id: string;
  actor_name: string;
  outcome: 'approved' | 'changes_requested';
  reason: string | null;
  created_at: string;
}

export interface PacketBundle {
  run_id: string;
  packet_id: string | null;
  latest: PacketRevision | null;
  revisions: PacketRevision[];
  reviews: PacketReview[];
  gate_binding: { gate_instance_id: string; packet_id: string; revision_id: string; revision: number; content_hash: string; completeness: string } | null;
  allowed_source_references: string[];
}


/** TJ-031/TJ-032 traceable structured task plan. The server validates the
 * exact approved packet trace and dependency DAG before persistence. */
export interface PlanTask {
  id: string;
  title: string;
  objective: string;
  requirement_refs: string[];
  enabling_rationale?: string | null;
  decision_refs: string[];
  dependencies: string[];
  acceptance_criteria: string[];
  components: string[];
  estimate: { size: string; uncertainty: string; rationale?: string | null };
  risks: { id: string; statement: string; severity: string; mitigation: string }[];
  verification: string[];
  notes?: string | null;
}

export interface PlanValidationReport {
  valid: boolean;
  task_count: number;
  requires_packet_revision: boolean;
  blocked_tasks: { task_id: string; reasons: string[] }[];
  open_decisions: string[];
}

export interface TaskReadiness {
  task_id: string;
  title: string;
  ready: boolean;
  dependencies: string[];
  requirement_refs: string[];
  acceptance_criteria: string[];
  components: string[];
  verification: string[];
  blocked_reasons: string[];
}

export interface PlanContent {
  schema_version?: number;
  title?: string;
  summary?: string;
  source_packet?: { packet_id: string; revision: number; content_hash: string } | null;
  tasks?: PlanTask[];
  scope_expansion_requests?: { id: string; description: string; rationale: string; requested_by: string }[];
  [key: string]: unknown;
}

export interface PlanRevision {
  id: string;
  plan_id: string;
  run_id: string;
  revision: number;
  schema_version: number;
  content_hash: string;
  packet_revision_id: string;
  packet_content_hash: string;
  packet_id: string;
  packet_revision: number;
  parent_revision_id: string | null;
  source_artifact_id: string | null;
  source_artifact_hash: string | null;
  flow_name: string | null;
  stage_name: string | null;
  change_request: string | null;
  created_by: string;
  created_at: string;
  content: PlanContent;
  validation?: PlanValidationReport;
  replayed?: boolean;
}

export interface PlanReview {
  id: string;
  plan_id: string;
  revision_id: string;
  revision: number;
  content_hash: string;
  packet_revision_id: string;
  packet_content_hash: string;
  gate_instance_id: string;
  decision_id: string;
  actor_id: string;
  actor_name: string;
  outcome: 'approved' | 'changes_requested';
  reason: string | null;
  created_at: string;
}

export interface PlanBundle {
  run_id: string;
  plan_id: string | null;
  latest: PlanRevision | null;
  approved: PlanRevision | null;
  revisions: PlanRevision[];
  reviews: PlanReview[];
  gate_binding: {
    gate_instance_id: string; plan_id: string; revision_id: string; revision: number;
    content_hash: string; packet_revision_id: string; packet_content_hash: string;
  } | null;
  readiness: TaskReadiness[];
  selections: { id: string; plan_revision_id: string; plan_content_hash: string; task_id: string; selected_by: string; created_at: string }[];
}

/** TJ-036 durable one-task publication identity and remote state. */
export interface RemotePublication {
  id: string; run_id: string; flow_name: string; attempt_id: string; operation_key: string;
  selection_id: string; task_id: string;
  plan_revision_id: string; plan_content_hash: string;
  packet_revision_id: string; packet_content_hash: string;
  branch: string; base_branch: string; workspace_path: string;
  base_sha: string | null; commit_sha: string | null;
  test_artifact_id: string; test_artifact_hash: string;
  review_artifact_id: string; review_artifact_hash: string;
  acceptance_artifact_id: string; acceptance_artifact_hash: string;
  scope_files_json: string; scope_violations_json: string;
  push_state: 'pending' | 'running' | 'succeeded' | 'failed' | 'unknown';
  push_error: string | null; remote_sha: string | null;
  pr_state: 'pending' | 'running' | 'succeeded' | 'failed' | 'unknown';
  pr_number: number | null; pr_url: string | null; pr_error: string | null;
  draft: boolean; created_at: string; updated_at: string;
}

export interface RunPublication {
  run_id: string; publication: RemotePublication | null; history: RemotePublication[];
  draft_pr: string | null; reconciliation_required: boolean;
}

/** TJ-039/TJ-040 external CI/CD delivery contract. TurboJet dispatches and
 * records evidence; the external system executes and owns deployment. */
export type DeliveryEnvironment = 'staging' | 'production';
export type DeliveryKind = 'deploy' | 'rollback';
export type DeliveryDispatchState = 'pending' | 'running' | 'succeeded' | 'failed' | 'unknown';
export type DeliveryCiState = 'not_started' | 'pending' | 'running' | 'succeeded' | 'failed' | 'cancelled' | 'unknown';
export type DeliveryDeploymentState = 'not_requested' | 'pending' | 'running' | 'succeeded' | 'failed' | 'unknown';
export type DeliveryVerificationState = 'pending' | 'passed' | 'failed' | 'unknown';
export type DeliveryObservationState = 'pending' | 'healthy' | 'degraded' | 'failed' | 'unknown';
export type DeliveryRecommendation = 'pending' | 'ready' | 'blocked';

export interface DeliveryRelease {
  id: string; run_id: string; project_id: string; publication_id: string;
  selection_id: string; task_id: string;
  plan_revision_id: string; plan_content_hash: string;
  packet_revision_id: string; packet_content_hash: string;
  pr_number: number | null; pr_url: string | null;
  commit_sha: string; base_sha: string | null; branch: string;
  environment: DeliveryEnvironment; kind: DeliveryKind;
  expected_revision: string; authorization_id: string | null; source_release_id: string | null;
  operation_key: string; workflow: string; workflow_ref: string;
  dispatch_state: DeliveryDispatchState;
  external_run_id: string | null; external_url: string | null; dispatch_error: string | null;
  ci_state: DeliveryCiState; ci_run_id: string | null; ci_url: string | null;
  deployment_state: DeliveryDeploymentState; deployed_revision: string | null;
  deployment_run_id: string | null; deployment_url: string | null; deployment_detail: string | null;
  verification_state: DeliveryVerificationState; verified_revision: string | null;
  verifier: string | null; verification_evidence_hash: string | null; verification_detail: string | null;
  observation_state: DeliveryObservationState; observation_evidence_hash: string | null; observation_detail: string | null;
  recommendation: DeliveryRecommendation; recommendation_reasons_json: string;
  last_sequence: number; last_error: string | null;
  created_by: string; created_at: string; updated_at: string;
}

export interface DeliveryDecision {
  id: string; run_id: string; publication_id: string;
  decision: 'authorize' | 'reject'; environment: DeliveryEnvironment; kind: DeliveryKind;
  commit_sha: string; expected_revision: string;
  staging_release_id: string | null; staging_verification_event_id: string | null;
  staging_observation_event_id: string | null; source_release_id: string | null;
  task_id: string; plan_revision_id: string; plan_content_hash: string;
  packet_revision_id: string; packet_content_hash: string;
  actor_id: string; actor_name: string; reason: string | null;
  idempotency_key: string; operation_key: string; created_at: string;
}

export interface DeliveryCallback {
  id: number; release_id: string; event_id: string; source: string; event_kind: string;
  sequence: number; payload_json: string; payload_hash: string;
  accepted: boolean; rejection_reason: string | null; received_at: string;
}

export interface DeliveryIntegration {
  configured: boolean; provider: string | null; repository?: string; workflow?: string;
  git_ref?: string; callback_window_secs?: number; callback_path_template?: string;
  execution_authority: string; limitation: string;
}

export interface DeliveryBundle {
  run_id: string; releases: DeliveryRelease[]; decisions: DeliveryDecision[];
  callbacks: DeliveryCallback[]; integration: DeliveryIntegration;
  execution_authority: string; note: string;
}

export interface RunOutput {
  run_id: string; profile: string | null; gate_name: string | null;
  gate_binding: GateRunBinding | null; latest: TjArtifact | null; decisions: GateDecision[];
}
