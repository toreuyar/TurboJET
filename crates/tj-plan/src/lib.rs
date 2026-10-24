//! Traceable structured task plan (TJ-031).
//!
//! A plan is the portable result of Planning. It is generated from one exact
//! approved requirements-packet revision, not from mutable prompt text. Every
//! task must either cite requirements from that packet or provide an explicit
//! enabling rationale. Dependencies form a checked DAG, open owned decisions
//! block the affected tasks with a visible reason, and scope expansion is a
//! request for a new packet revision rather than an invented requirement.
//!
//! This crate intentionally has no database/runtime dependency: the same
//! validation, readiness and export contract is used by persistence, the API
//! and offline consumers.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

pub const PLAN_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskPlan {
    pub schema_version: u32,
    pub title: String,
    pub summary: String,
    /// Exact approved packet identity the plan was generated from. The runtime
    /// fills this in when the model omits it and rejects a mismatching claim;
    /// it is never inferred from a mutable "latest packet" read.
    #[serde(default)]
    pub source_packet: Option<PlanSourcePacket>,
    #[serde(default)]
    pub tasks: Vec<PlanTask>,
    /// Scope that is intentionally *not* turned into tasks. Each request tells
    /// the owner to create a new requirements-packet revision and re-approve.
    #[serde(default)]
    pub scope_expansion_requests: Vec<ScopeExpansionRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PlanSourcePacket {
    pub packet_id: String,
    pub revision: i64,
    pub content_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PlanTask {
    pub id: String,
    pub title: String,
    pub objective: String,
    /// Packet requirement/NFR/constraint IDs this task implements.
    #[serde(default)]
    pub requirement_refs: Vec<String>,
    /// Why this task is necessary when it is enabling work rather than a
    /// direct requirement. Empty refs plus empty rationale is invalid.
    #[serde(default)]
    pub enabling_rationale: Option<String>,
    /// Owned packet decisions that affect this task. An open decision makes
    /// the task visibly blocked.
    #[serde(default)]
    pub decision_refs: Vec<String>,
    /// Other task IDs. Missing, self and cyclic references are invalid.
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub components: Vec<String>,
    pub estimate: TaskEstimate,
    #[serde(default)]
    pub risks: Vec<TaskRisk>,
    #[serde(default)]
    pub verification: Vec<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskEstimate {
    /// xs, s, m, l or xl.
    pub size: String,
    /// low, medium or high.
    pub uncertainty: String,
    #[serde(default)]
    pub rationale: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RiskSeverity {
    Critical,
    Major,
    Minor,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskRisk {
    pub id: String,
    pub statement: String,
    pub severity: RiskSeverity,
    pub mitigation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScopeExpansionRequest {
    pub id: String,
    pub description: String,
    pub rationale: String,
    pub requested_by: String,
}

/// Packet facts used to validate traces and decision blocks. It is built from
/// the exact approved revision, never from a caller-supplied "current" packet.
#[derive(Debug, Clone, Default)]
pub struct PlanContext {
    pub requirement_ids: HashSet<String>,
    /// Decision ID -> question, for open decisions only.
    pub open_decisions: HashMap<String, String>,
    /// All owned decision IDs (open and decided), for reference validation.
    pub decision_ids: HashSet<String>,
}

impl PlanContext {
    pub fn from_packet_value(packet: &serde_json::Value) -> Self {
        let mut context = Self::default();
        for key in ["requirements", "nfrs", "constraints"] {
            if let Some(items) = packet.get(key).and_then(|value| value.as_array()) {
                for item in items {
                    if let Some(id) = item.get("id").and_then(|value| value.as_str()) {
                        context.requirement_ids.insert(id.to_string());
                    }
                }
            }
        }
        if let Some(items) = packet.get("decisions").and_then(|value| value.as_array()) {
            for item in items {
                let Some(id) = item.get("id").and_then(|value| value.as_str()) else {
                    continue;
                };
                context.decision_ids.insert(id.to_string());
                if item.get("status").and_then(|value| value.as_str()) == Some("open") {
                    let question = item
                        .get("question")
                        .and_then(|value| value.as_str())
                        .unwrap_or("open owned decision");
                    context
                        .open_decisions
                        .insert(id.to_string(), question.to_string());
                }
            }
        }
        context
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskBlock {
    pub task_id: String,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanValidationReport {
    pub valid: bool,
    pub task_count: usize,
    /// True when the plan records scope that needs a new packet revision. The
    /// plan stays valid and exportable; affected work is not a task.
    pub requires_packet_revision: bool,
    pub blocked_tasks: Vec<TaskBlock>,
    pub open_decisions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskReadiness {
    pub task_id: String,
    pub title: String,
    pub ready: bool,
    pub dependencies: Vec<String>,
    pub requirement_refs: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub components: Vec<String>,
    pub verification: Vec<String>,
    pub blocked_reasons: Vec<String>,
}

impl TaskPlan {
    pub fn from_json(raw: &str) -> Result<Self, PlanError> {
        let plan: Self =
            serde_json::from_str(raw).map_err(|error| PlanError::Json(error.to_string()))?;
        plan.validate(None)?;
        Ok(plan)
    }

    /// Structural validation plus, when a context is supplied, trace and
    /// decision checks against the exact approved packet revision.
    pub fn validate(
        &self,
        context: Option<&PlanContext>,
    ) -> Result<PlanValidationReport, PlanError> {
        let invalid = |message: String| Err(PlanError::Invalid(message));
        if self.schema_version != PLAN_SCHEMA_VERSION {
            return invalid(format!(
                "unsupported schema_version {} (expected {})",
                self.schema_version, PLAN_SCHEMA_VERSION
            ));
        }
        if self.title.trim().is_empty() {
            return invalid("title is required".into());
        }
        if self.summary.trim().is_empty() {
            return invalid("summary is required".into());
        }
        if self.tasks.is_empty() {
            return invalid("a plan requires at least one task".into());
        }
        if let Some(source) = &self.source_packet {
            if source.packet_id.trim().is_empty() {
                return invalid("source_packet.packet_id is required".into());
            }
            if source.revision <= 0 {
                return invalid("source_packet.revision must be positive".into());
            }
            if source.content_hash.len() != 64
                || !source.content_hash.chars().all(|ch| ch.is_ascii_hexdigit())
            {
                return invalid(
                    "source_packet.content_hash must be a 64-character SHA-256 hex digest".into(),
                );
            }
        }

        let mut task_ids = HashSet::new();
        for task in &self.tasks {
            if task.id.trim().is_empty() {
                return invalid("task id is required".into());
            }
            if !task_ids.insert(task.id.as_str()) {
                return invalid(format!("duplicate task id '{}'", task.id));
            }
        }
        for task in &self.tasks {
            if task.title.trim().is_empty() || task.objective.trim().is_empty() {
                return invalid(format!("task '{}' requires title and objective", task.id));
            }
            if task.acceptance_criteria.is_empty()
                || task
                    .acceptance_criteria
                    .iter()
                    .any(|item| item.trim().is_empty())
            {
                return invalid(format!(
                    "task '{}' requires at least one non-empty acceptance criterion",
                    task.id
                ));
            }
            if task.components.is_empty()
                || task.components.iter().any(|item| item.trim().is_empty())
            {
                return invalid(format!(
                    "task '{}' requires at least one component",
                    task.id
                ));
            }
            if task.verification.is_empty()
                || task.verification.iter().any(|item| item.trim().is_empty())
            {
                return invalid(format!(
                    "task '{}' requires at least one verification step",
                    task.id
                ));
            }
            let size = task.estimate.size.to_ascii_lowercase();
            if !matches!(size.as_str(), "xs" | "s" | "m" | "l" | "xl") {
                return invalid(format!(
                    "task '{}' estimate.size must be xs, s, m, l or xl",
                    task.id
                ));
            }
            let uncertainty = task.estimate.uncertainty.to_ascii_lowercase();
            if !matches!(uncertainty.as_str(), "low" | "medium" | "high") {
                return invalid(format!(
                    "task '{}' estimate.uncertainty must be low, medium or high",
                    task.id
                ));
            }
            if task.requirement_refs.is_empty()
                && task
                    .enabling_rationale
                    .as_deref()
                    .is_none_or(|value| value.trim().is_empty())
            {
                return invalid(format!(
                    "task '{}' must link a packet requirement or provide an explicit enabling rationale",
                    task.id
                ));
            }
            let mut seen_decisions = HashSet::new();
            for reference in &task.decision_refs {
                if !seen_decisions.insert(reference) {
                    return invalid(format!(
                        "task '{}' repeats decision reference '{}'",
                        task.id, reference
                    ));
                }
            }
            let mut seen_dependencies = HashSet::new();
            for dependency in &task.dependencies {
                if dependency == &task.id {
                    return invalid(format!("task '{}' cannot depend on itself", task.id));
                }
                if !task_ids.contains(dependency.as_str()) {
                    return invalid(format!(
                        "task '{}' depends on missing task '{}'",
                        task.id, dependency
                    ));
                }
                if !seen_dependencies.insert(dependency) {
                    return invalid(format!(
                        "task '{}' repeats dependency '{}'",
                        task.id, dependency
                    ));
                }
            }
            let mut risk_ids = HashSet::new();
            for risk in &task.risks {
                if risk.id.trim().is_empty() || !risk_ids.insert(risk.id.as_str()) {
                    return invalid(format!(
                        "task '{}' has an empty or duplicate risk id",
                        task.id
                    ));
                }
                if risk.statement.trim().is_empty() || risk.mitigation.trim().is_empty() {
                    return invalid(format!(
                        "risk '{}' in task '{}' requires statement and mitigation",
                        risk.id, task.id
                    ));
                }
            }
        }
        detect_cycle(&self.tasks)?;

        let mut requires_packet_revision = false;
        let mut expansion_ids = HashSet::new();
        for request in &self.scope_expansion_requests {
            if request.id.trim().is_empty() || !expansion_ids.insert(request.id.as_str()) {
                return invalid("scope expansion request id is empty or duplicated".into());
            }
            if request.description.trim().is_empty()
                || request.rationale.trim().is_empty()
                || request.requested_by.trim().is_empty()
            {
                return invalid(format!(
                    "scope expansion request '{}' requires description, rationale and requester",
                    request.id
                ));
            }
            requires_packet_revision = true;
        }

        let mut blocked_tasks = Vec::new();
        let mut open_decisions = context
            .map(|ctx| ctx.open_decisions.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        open_decisions.sort();
        if let Some(context) = context {
            for task in &self.tasks {
                for reference in &task.requirement_refs {
                    if !context.requirement_ids.contains(reference) {
                        return invalid(format!(
                            "task '{}' cites unknown requirement '{}'; new scope requires a requirements packet revision, not an invented requirement",
                            task.id, reference
                        ));
                    }
                }
                let mut reasons = Vec::new();
                for reference in &task.decision_refs {
                    if !context.decision_ids.contains(reference) {
                        return invalid(format!(
                            "task '{}' cites unknown owned decision '{}' from the approved packet",
                            task.id, reference
                        ));
                    }
                    if let Some(question) = context.open_decisions.get(reference) {
                        reasons.push(format!(
                            "blocked by open decision {}: {}",
                            reference, question
                        ));
                    }
                }
                if !reasons.is_empty() {
                    blocked_tasks.push(TaskBlock {
                        task_id: task.id.clone(),
                        reasons,
                    });
                }
            }
        }

        Ok(PlanValidationReport {
            valid: true,
            task_count: self.tasks.len(),
            requires_packet_revision,
            blocked_tasks,
            open_decisions,
        })
    }

    /// Ready/blocked contract for Engineering. `completed` is the durable set
    /// of completed task IDs; an empty set is the normal review-time view.
    pub fn readiness(
        &self,
        completed: &HashSet<String>,
        context: Option<&PlanContext>,
    ) -> Vec<TaskReadiness> {
        let completed: HashSet<&str> = completed.iter().map(String::as_str).collect();
        self.tasks
            .iter()
            .map(|task| {
                let mut reasons = Vec::new();
                for dependency in &task.dependencies {
                    if !completed.contains(dependency.as_str()) {
                        reasons.push(format!("dependency '{}' is not complete", dependency));
                    }
                }
                if let Some(context) = context {
                    for reference in &task.decision_refs {
                        if let Some(question) = context.open_decisions.get(reference) {
                            reasons.push(format!(
                                "blocked by open decision {}: {}",
                                reference, question
                            ));
                        }
                    }
                }
                TaskReadiness {
                    task_id: task.id.clone(),
                    title: task.title.clone(),
                    ready: reasons.is_empty(),
                    dependencies: task.dependencies.clone(),
                    requirement_refs: task.requirement_refs.clone(),
                    acceptance_criteria: task.acceptance_criteria.clone(),
                    components: task.components.clone(),
                    verification: task.verification.clone(),
                    blocked_reasons: reasons,
                }
            })
            .collect()
    }

    /// Canonical content hash with deterministic field order for a given value.
    pub fn content_hash(&self) -> String {
        let bytes = serde_json::to_vec(self).unwrap_or_default();
        format!("{:x}", Sha256::digest(bytes))
    }

    pub fn task(&self, task_id: &str) -> Option<&PlanTask> {
        self.tasks.iter().find(|task| task.id == task_id)
    }
}

fn detect_cycle(tasks: &[PlanTask]) -> Result<(), PlanError> {
    let index: HashMap<&str, usize> = tasks
        .iter()
        .enumerate()
        .map(|(i, task)| (task.id.as_str(), i))
        .collect();
    // 0 = unvisited, 1 = visiting, 2 = done.
    let mut state = vec![0u8; tasks.len()];
    fn visit(
        index: usize,
        tasks: &[PlanTask],
        lookup: &HashMap<&str, usize>,
        state: &mut [u8],
        stack: &mut Vec<String>,
    ) -> Result<(), PlanError> {
        if state[index] == 1 {
            let start = stack
                .iter()
                .position(|id| id == &tasks[index].id)
                .unwrap_or(0);
            let mut cycle = stack[start..].to_vec();
            cycle.push(tasks[index].id.clone());
            return Err(PlanError::Invalid(format!(
                "cyclic task dependency: {}",
                cycle.join(" -> ")
            )));
        }
        if state[index] == 2 {
            return Ok(());
        }
        state[index] = 1;
        stack.push(tasks[index].id.clone());
        for dependency in &tasks[index].dependencies {
            let next = *lookup
                .get(dependency.as_str())
                .expect("dependency existence validated");
            visit(next, tasks, lookup, state, stack)?;
        }
        stack.pop();
        state[index] = 2;
        Ok(())
    }
    let mut stack = Vec::new();
    for task_index in 0..tasks.len() {
        visit(task_index, tasks, &index, &mut state, &mut stack)?;
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error("plan is not valid JSON for schema v1: {0}")]
    Json(String),
    #[error("invalid task plan: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanProvenance {
    pub run_id: String,
    pub flow_name: Option<String>,
    pub stage_name: Option<String>,
    pub artifact_id: Option<String>,
    pub artifact_hash: Option<String>,
    pub created_by: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanReviewRecord {
    pub id: String,
    pub gate_instance_id: String,
    pub actor_id: String,
    pub actor_name: String,
    pub outcome: String,
    pub reason: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanRevisionSummary {
    pub revision_id: String,
    pub revision: i64,
    pub content_hash: String,
    pub parent_revision_id: Option<String>,
    pub packet_revision_id: String,
    pub packet_content_hash: String,
    pub change_request: Option<String>,
    pub created_by: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanDocument {
    pub plan_id: String,
    pub revision: i64,
    pub content_hash: String,
    pub packet_revision_id: String,
    pub packet_content_hash: String,
    pub plan: TaskPlan,
    pub validation: PlanValidationReport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanEnvelope {
    pub export_schema_version: u32,
    pub plan: PlanDocument,
    pub provenance: PlanProvenance,
    pub reviews: Vec<PlanReviewRecord>,
    pub history: Vec<PlanRevisionSummary>,
}

impl PlanEnvelope {
    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    pub fn to_markdown(&self) -> String {
        let plan = &self.plan.plan;
        let mut out = String::new();
        out.push_str(&format!("# {}\n\n", plan.title));
        out.push_str(&format!(
            "> Structured task plan v{} · `{}` revision {} · `{}`\n\n",
            plan.schema_version, self.plan.plan_id, self.plan.revision, self.plan.content_hash
        ));
        out.push_str("## Portable plan metadata\n\n");
        out.push_str(&format!("- Plan ID: `{}`\n", self.plan.plan_id));
        out.push_str(&format!("- Revision: {}\n", self.plan.revision));
        out.push_str(&format!("- Content hash: `{}`\n", self.plan.content_hash));
        out.push_str(&format!(
            "- Source packet revision: `{}`\n",
            self.plan.packet_revision_id
        ));
        out.push_str(&format!(
            "- Source packet hash: `{}`\n",
            self.plan.packet_content_hash
        ));
        if let Some(source) = &plan.source_packet {
            out.push_str(&format!(
                "- Packet identity: `{}` r{} `{}`\n",
                source.packet_id, source.revision, source.content_hash
            ));
        }
        out.push_str(&format!(
            "- Validation: **valid** · {} task(s) · requires packet revision: {}\n",
            self.plan.validation.task_count, self.plan.validation.requires_packet_revision
        ));
        out.push_str(&format!("- Run: `{}`\n", self.provenance.run_id));
        if let Some(flow) = &self.provenance.flow_name {
            out.push_str(&format!("- Generated in flow: `{}`\n", flow));
        }
        if let Some(stage) = &self.provenance.stage_name {
            out.push_str(&format!("- Generated by stage: `{}`\n", stage));
        }
        out.push_str(&format!(
            "- Provenance actor: `{}` at `{}`\n",
            self.provenance.created_by, self.provenance.created_at
        ));
        if let Some(hash) = &self.provenance.artifact_hash {
            out.push_str(&format!("- Source artifact hash: `{}`\n", hash));
        }

        out.push_str("\n## Summary\n\n");
        out.push_str(&plan.summary);
        out.push_str("\n\n## Tasks\n\n");
        for task in &plan.tasks {
            out.push_str(&format!(
                "### {} — {}\n\n{}\n\n",
                task.id, task.title, task.objective
            ));
            out.push_str(&format!(
                "- Estimate: `{}` · uncertainty `{}`\n",
                task.estimate.size, task.estimate.uncertainty
            ));
            out.push_str(&format!("- Components: {}\n", task.components.join(", ")));
            if !task.requirement_refs.is_empty() {
                out.push_str(&format!(
                    "- Requirements: {}\n",
                    task.requirement_refs.join(", ")
                ));
            }
            if let Some(rationale) = &task.enabling_rationale {
                out.push_str(&format!("- Enabling rationale: {}\n", rationale));
            }
            if !task.dependencies.is_empty() {
                out.push_str(&format!("- Depends on: {}\n", task.dependencies.join(", ")));
            }
            if !task.decision_refs.is_empty() {
                out.push_str(&format!(
                    "- Decision references: {}\n",
                    task.decision_refs.join(", ")
                ));
            }
            for criterion in &task.acceptance_criteria {
                out.push_str(&format!("- Acceptance: {}\n", criterion));
            }
            for step in &task.verification {
                out.push_str(&format!("- Verification: {}\n", step));
            }
            for risk in &task.risks {
                out.push_str(&format!(
                    "- Risk {} ({:?}): {} — mitigation: {}\n",
                    risk.id, risk.severity, risk.statement, risk.mitigation
                ));
            }
            out.push('\n');
        }

        out.push_str("## Blocked tasks\n\n");
        if self.plan.validation.blocked_tasks.is_empty() {
            out.push_str("_None._\n\n");
        } else {
            for block in &self.plan.validation.blocked_tasks {
                out.push_str(&format!(
                    "- `{}`: {}\n",
                    block.task_id,
                    block.reasons.join("; ")
                ));
            }
            out.push('\n');
        }

        out.push_str("## Scope expansion requests\n\n");
        if plan.scope_expansion_requests.is_empty() {
            out.push_str("_None. No new scope was invented._\n\n");
        } else {
            out.push_str("> These requests require a new approved requirements packet revision before they become tasks.\n\n");
            for request in &plan.scope_expansion_requests {
                out.push_str(&format!(
                    "- `{}` ({}) — {}\n",
                    request.id, request.requested_by, request.description
                ));
            }
            out.push('\n');
        }

        out.push_str("## Approval and revision history\n\n");
        if self.history.is_empty() {
            out.push_str("_No revision history._\n");
        }
        for item in &self.history {
            out.push_str(&format!(
                "- r{} `{}` — {} by {} at {}; packet `{}`{}{}\n",
                item.revision,
                item.revision_id,
                item.content_hash,
                item.created_by,
                item.created_at,
                item.packet_revision_id,
                item.change_request
                    .as_ref()
                    .map(|reason| format!(" — change requested: {reason}"))
                    .unwrap_or_default(),
                ""
            ));
        }
        if self.reviews.is_empty() {
            out.push_str("\n_No gate reviews recorded._\n");
        } else {
            for review in &self.reviews {
                out.push_str(&format!(
                    "- {} by {} at {} via gate `{}`{}\n",
                    review.outcome,
                    review.actor_name,
                    review.created_at,
                    review.gate_instance_id,
                    review
                        .reason
                        .as_ref()
                        .map(|reason| format!(" — {reason}"))
                        .unwrap_or_default()
                ));
            }
        }
        out
    }
}

/// Line-level diff shared by the reviewer UI and tests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffKind {
    Same,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
    pub kind: DiffKind,
    pub text: String,
}

pub fn diff_lines(before: &str, after: &str) -> Vec<DiffLine> {
    let a: Vec<&str> = before.lines().collect();
    let b: Vec<&str> = after.lines().collect();
    let mut table = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            table[i][j] = if a[i] == b[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            out.push(DiffLine {
                kind: DiffKind::Same,
                text: a[i].to_string(),
            });
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            out.push(DiffLine {
                kind: DiffKind::Removed,
                text: a[i].to_string(),
            });
            i += 1;
        } else {
            out.push(DiffLine {
                kind: DiffKind::Added,
                text: b[j].to_string(),
            });
            j += 1;
        }
    }
    while i < a.len() {
        out.push(DiffLine {
            kind: DiffKind::Removed,
            text: a[i].to_string(),
        });
        i += 1;
    }
    while j < b.len() {
        out.push(DiffLine {
            kind: DiffKind::Added,
            text: b[j].to_string(),
        });
        j += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMPLETE: &str = include_str!("../fixtures/complete.json");
    const BLOCKED: &str = include_str!("../fixtures/blocked-open-decision.json");

    fn context() -> PlanContext {
        PlanContext::from_packet_value(&serde_json::json!({
            "requirements": [
                {"id": "REQ-1"}, {"id": "REQ-2"}, {"id": "REQ-3"}
            ],
            "nfrs": [{"id": "NFR-1"}],
            "constraints": [{"id": "CON-1"}],
            "decisions": [
                {"id": "DEC-1", "status": "decided", "question": "Partial refunds?"},
                {"id": "DEC-2", "status": "open", "question": "Which notification channel?"}
            ]
        }))
    }

    #[test]
    fn complete_fixture_is_valid_traceable_and_hash_stable() {
        let plan: TaskPlan = serde_json::from_str(COMPLETE).unwrap();
        let report = plan.validate(Some(&context())).unwrap();
        assert_eq!(report.task_count, 4);
        assert!(report.blocked_tasks.is_empty());
        assert!(!report.requires_packet_revision);
        assert_eq!(plan.content_hash(), plan.content_hash());
        assert_eq!(plan.content_hash().len(), 64);
        let changed = COMPLETE.replace(
            "Cancellation eligibility and preview contract",
            "Cancellation eligibility and refund preview contract",
        );
        let changed: TaskPlan = serde_json::from_str(&changed).unwrap();
        assert_ne!(plan.content_hash(), changed.content_hash());
    }

    #[test]
    fn every_task_needs_a_requirement_trace_or_explicit_enabling_rationale() {
        let mut plan: TaskPlan = serde_json::from_str(COMPLETE).unwrap();
        plan.tasks[0].requirement_refs.clear();
        plan.tasks[0].enabling_rationale = None;
        let error = plan.validate(Some(&context())).unwrap_err().to_string();
        assert!(error.contains("enabling rationale"), "{error}");

        plan.tasks[0].enabling_rationale =
            Some("Shared test harness is required by all traced tasks.".into());
        plan.validate(Some(&context())).unwrap();
    }

    #[test]
    fn unknown_requirement_is_a_scope_expansion_not_an_invented_requirement() {
        let mut plan: TaskPlan = serde_json::from_str(COMPLETE).unwrap();
        plan.tasks[0].requirement_refs.push("REQ-NEW".into());
        let error = plan.validate(Some(&context())).unwrap_err().to_string();
        assert!(error.contains("unknown requirement 'REQ-NEW'"), "{error}");
        assert!(error.contains("requirements packet revision"), "{error}");
    }

    #[test]
    fn missing_duplicate_and_cyclic_dependencies_are_rejected() {
        let mut plan: TaskPlan = serde_json::from_str(COMPLETE).unwrap();
        plan.tasks[0].dependencies.push("T-MISSING".into());
        assert!(
            plan.validate(None)
                .unwrap_err()
                .to_string()
                .contains("missing task")
        );
        plan.tasks[0].dependencies.clear();

        plan.tasks[1].dependencies = vec!["T-1".into()];
        plan.tasks[0].dependencies = vec!["T-2".into()];
        let error = plan.validate(None).unwrap_err().to_string();
        assert!(error.contains("cyclic task dependency"), "{error}");

        plan.tasks[0].dependencies = vec!["T-1".into()];
        assert!(
            plan.validate(None)
                .unwrap_err()
                .to_string()
                .contains("itself")
        );
    }

    #[test]
    fn open_owned_decisions_block_affected_tasks_visibly() {
        let plan: TaskPlan = serde_json::from_str(BLOCKED).unwrap();
        let report = plan.validate(Some(&context())).unwrap();
        assert_eq!(report.open_decisions, vec!["DEC-2".to_string()]);
        assert!(report.blocked_tasks.iter().any(|block| {
            block.task_id == "T-2"
                && block
                    .reasons
                    .iter()
                    .any(|reason| reason.contains("blocked by open decision DEC-2"))
        }));
        let readiness = plan.readiness(&HashSet::new(), Some(&context()));
        assert!(
            !readiness
                .iter()
                .find(|task| task.task_id == "T-2")
                .unwrap()
                .ready
        );
    }

    #[test]
    fn readiness_requires_completed_dependencies_and_no_open_decision() {
        let plan: TaskPlan = serde_json::from_str(COMPLETE).unwrap();
        let readiness = plan.readiness(&HashSet::new(), Some(&context()));
        assert!(
            readiness
                .iter()
                .find(|task| task.task_id == "T-1")
                .unwrap()
                .ready
        );
        let blocked = readiness.iter().find(|task| task.task_id == "T-2").unwrap();
        assert!(!blocked.ready);
        assert!(
            blocked
                .blocked_reasons
                .iter()
                .any(|reason| reason.contains("dependency 'T-1'"))
        );

        let completed: HashSet<String> = ["T-1".to_string()].into_iter().collect();
        let readiness = plan.readiness(&completed, Some(&context()));
        assert!(
            readiness
                .iter()
                .find(|task| task.task_id == "T-2")
                .unwrap()
                .ready
        );
    }

    #[test]
    fn scope_expansion_requests_are_explicit_and_never_tasks() {
        let mut plan: TaskPlan = serde_json::from_str(COMPLETE).unwrap();
        plan.scope_expansion_requests.push(ScopeExpansionRequest {
            id: "EXP-1".into(),
            description: "Partial refunds".into(),
            rationale:
                "A customer asked for them, but the approved packet excludes partial refunds."
                    .into(),
            requested_by: "planning-review".into(),
        });
        let report = plan.validate(Some(&context())).unwrap();
        assert!(report.requires_packet_revision);
        assert_eq!(plan.tasks.len(), 4);
    }

    #[test]
    fn portable_markdown_and_json_carry_the_same_identity_and_provenance() {
        let plan: TaskPlan = serde_json::from_str(COMPLETE).unwrap();
        let report = plan.validate(Some(&context())).unwrap();
        let envelope = PlanEnvelope {
            export_schema_version: 1,
            plan: PlanDocument {
                plan_id: "pln_example".into(),
                revision: 2,
                content_hash: plan.content_hash(),
                packet_revision_id: "pr_example".into(),
                packet_content_hash: "b".repeat(64),
                plan: plan.clone(),
                validation: report,
            },
            provenance: PlanProvenance {
                run_id: "run_example".into(),
                flow_name: Some("planning".into()),
                stage_name: Some("plan".into()),
                artifact_id: Some("art_plan".into()),
                artifact_hash: Some("c".repeat(64)),
                created_by: "system:plan-generation".into(),
                created_at: "2026-10-22T00:00:00Z".into(),
            },
            reviews: vec![],
            history: vec![],
        };
        let json = envelope.to_json_pretty();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["plan"]["content_hash"], plan.content_hash());
        assert_eq!(parsed["plan"]["packet_revision_id"], "pr_example");
        let markdown = envelope.to_markdown();
        assert!(markdown.contains(&plan.content_hash()));
        assert!(markdown.contains("Source packet hash"));
        assert!(markdown.contains("REQ-1"));
        assert!(markdown.contains("Depends on"));
        assert!(markdown.contains("Scope expansion requests"));
    }

    #[test]
    fn structural_rejection_is_precise() {
        let mut plan: TaskPlan = serde_json::from_str(COMPLETE).unwrap();
        plan.tasks[0].acceptance_criteria.clear();
        assert!(
            plan.validate(None)
                .unwrap_err()
                .to_string()
                .contains("acceptance criterion")
        );
        plan.tasks[0].acceptance_criteria.push("x".into());
        plan.tasks[1].acceptance_criteria.clear();
        plan.tasks[1].acceptance_criteria.push("x".into());
        plan.tasks[2].acceptance_criteria.clear();
        plan.tasks[2].acceptance_criteria.push("x".into());
        plan.tasks[3].acceptance_criteria.clear();
        plan.tasks[3].acceptance_criteria.push("x".into());
        plan.tasks.retain(|task| task.id != "T-4");
        let raw = serde_json::to_string(&plan)
            .unwrap()
            .replace("\"tasks\":[", "\"tasks\":[{\"id\":\"T-x\"},");
        assert!(TaskPlan::from_json(&raw).is_err());
    }
}
