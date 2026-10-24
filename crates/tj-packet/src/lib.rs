//! Requirements packet schema v1 (TJ-020).
//!
//! A packet is the portable, evidence-linked result of Discovery. It contains
//! the problem, users, goals/non-goals, requirements, NFRs, constraints,
//! evidence, assumptions, contradictions, risks and explicitly owned open
//! decisions. Confirmed requirements must cite known sources; unknown facts
//! stay unknown instead of being invented.
//!
//! The crate deliberately has no database/runtime dependency so the same
//! validation and rendering contract is used by persistence, the API and
//! offline export.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

pub const PACKET_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RequirementsPacket {
    pub schema_version: u32,
    pub title: String,
    pub problem: ProblemStatement,
    #[serde(default)]
    pub users: Vec<UserNeed>,
    #[serde(default)]
    pub goals: Vec<Goal>,
    #[serde(default)]
    pub non_goals: Vec<NonGoal>,
    #[serde(default)]
    pub requirements: Vec<Requirement>,
    #[serde(default)]
    pub nfrs: Vec<NonFunctionalRequirement>,
    #[serde(default)]
    pub constraints: Vec<Constraint>,
    #[serde(default)]
    pub evidence: Vec<Evidence>,
    #[serde(default)]
    pub assumptions: Vec<Assumption>,
    #[serde(default)]
    pub contradictions: Vec<Contradiction>,
    #[serde(default)]
    pub risks: Vec<Risk>,
    #[serde(default)]
    pub decisions: Vec<OwnedDecision>,
    #[serde(default)]
    pub unknowns: Vec<Unknown>,
    #[serde(default)]
    pub sources: Vec<Source>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProblemStatement {
    pub statement: String,
    #[serde(default)]
    pub source_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UserNeed {
    pub id: String,
    pub user: String,
    pub need: String,
    #[serde(default)]
    pub source_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Goal {
    pub id: String,
    pub statement: String,
    #[serde(default)]
    pub source_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NonGoal {
    pub id: String,
    pub statement: String,
    #[serde(default)]
    pub source_refs: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RequirementKind {
    Functional,
    NonFunctional,
    Constraint,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RequirementStatus {
    Confirmed,
    Proposed,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Must,
    Should,
    Could,
    Wont,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Requirement {
    pub id: String,
    pub title: String,
    pub statement: String,
    pub kind: RequirementKind,
    pub priority: Priority,
    pub status: RequirementStatus,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub source_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NonFunctionalRequirement {
    pub id: String,
    pub statement: String,
    pub category: String,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub source_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Constraint {
    pub id: String,
    pub statement: String,
    #[serde(default)]
    pub source_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub id: String,
    pub claim: String,
    #[serde(default)]
    pub source_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Assumption {
    pub id: String,
    pub statement: String,
    pub owner: String,
    #[serde(default)]
    pub source_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Contradiction {
    pub id: String,
    pub statement: String,
    pub owner: String,
    #[serde(default)]
    pub source_refs: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Critical,
    Major,
    Minor,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Likelihood {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Risk {
    pub id: String,
    pub statement: String,
    pub severity: Severity,
    pub likelihood: Likelihood,
    pub mitigation: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DecisionStatus {
    Open,
    Decided,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OwnedDecision {
    pub id: String,
    pub question: String,
    pub owner: String,
    pub status: DecisionStatus,
    #[serde(default)]
    pub decision: Option<String>,
    #[serde(default)]
    pub source_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Unknown {
    pub id: String,
    pub question: String,
    pub impact: String,
    pub owner: String,
    #[serde(default)]
    pub source_refs: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// Raw request supplied when the run was created.
    UserInput,
    /// A durable answer accepted for a stage question.
    QuestionAnswer,
    /// A frozen stage artifact produced during this run.
    StageArtifact,
    /// A decision owned by a named human (the run/actor is the reference).
    OwnerDecision,
    /// A document supplied outside the run; never sufficient on its own for a
    /// confirmed requirement in the supported Discovery profile.
    ExternalDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub id: String,
    pub kind: SourceKind,
    /// Stable reference: `artifact:<flow>:<stage>`, `answer:<question-id>`,
    /// `run:<run-id>` or an explicit external document identifier.
    pub reference: String,
    pub excerpt: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Completeness {
    Complete,
    Incomplete,
    Contradictory,
}

impl Completeness {
    pub fn as_str(self) -> &'static str {
        match self {
            Completeness::Complete => "complete",
            Completeness::Incomplete => "incomplete",
            Completeness::Contradictory => "contradictory",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PacketProvenance {
    pub run_id: String,
    pub flow_name: Option<String>,
    pub stage_name: Option<String>,
    pub artifact_id: Option<String>,
    pub artifact_hash: Option<String>,
    pub created_by: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PacketReviewRecord {
    pub id: String,
    pub gate_instance_id: String,
    pub actor_id: String,
    pub actor_name: String,
    pub outcome: String,
    pub reason: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PacketExportMeta {
    pub packet_id: String,
    pub revision: i64,
    pub content_hash: String,
    pub completeness: Completeness,
    pub provenance: PacketProvenance,
    pub reviews: Vec<PacketReviewRecord>,
    pub history: Vec<PacketRevisionSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PacketRevisionSummary {
    pub revision_id: String,
    pub revision: i64,
    pub content_hash: String,
    pub completeness: Completeness,
    pub parent_revision_id: Option<String>,
    pub change_request: Option<String>,
    pub created_by: String,
    pub created_at: String,
}

#[derive(Debug, thiserror::Error)]
pub enum PacketError {
    #[error("packet is not valid JSON for schema v1: {0}")]
    Json(String),
    #[error("invalid requirements packet: {0}")]
    Invalid(String),
}

impl RequirementsPacket {
    pub fn from_json(raw: &str) -> Result<Self, PacketError> {
        let packet: Self =
            serde_json::from_str(raw).map_err(|error| PacketError::Json(error.to_string()))?;
        packet.validate()?;
        Ok(packet)
    }

    pub fn validate(&self) -> Result<(), PacketError> {
        let invalid = |message: String| Err(PacketError::Invalid(message));
        if self.schema_version != PACKET_SCHEMA_VERSION {
            return invalid(format!(
                "unsupported schema_version {} (expected {})",
                self.schema_version, PACKET_SCHEMA_VERSION
            ));
        }
        if self.title.trim().is_empty() {
            return invalid("title is required".into());
        }
        if self.problem.statement.trim().is_empty() {
            return invalid("problem.statement is required".into());
        }
        if self.problem.source_refs.is_empty() {
            return invalid("problem.source_refs must cite at least one source".into());
        }

        let mut source_ids = HashSet::new();
        for source in &self.sources {
            if source.id.trim().is_empty() || !source_ids.insert(source.id.as_str()) {
                return invalid(format!("source id '{}' is empty or duplicated", source.id));
            }
            if source.reference.trim().is_empty() {
                return invalid(format!("source '{}' has an empty reference", source.id));
            }
            if source.excerpt.trim().is_empty() {
                return invalid(format!("source '{}' has an empty excerpt", source.id));
            }
        }

        let mut ids = HashSet::new();
        let mut check_id = |kind: &str, id: &str| -> Result<(), PacketError> {
            if id.trim().is_empty() {
                return invalid(format!("{kind} id is required"));
            }
            if !ids.insert(id.to_string()) {
                return invalid(format!("duplicate {kind} id '{id}'"));
            }
            Ok(())
        };
        let resolve = |refs: &[String], what: &str| -> Result<(), PacketError> {
            for reference in refs {
                if !source_ids.contains(reference.as_str()) {
                    return invalid(format!(
                        "{what} cites unknown source '{}'; confirmed facts cannot use invented evidence",
                        reference
                    ));
                }
            }
            Ok(())
        };

        resolve(&self.problem.source_refs, "problem")?;
        for item in &self.users {
            check_id("user", &item.id)?;
            if item.user.trim().is_empty() || item.need.trim().is_empty() {
                return invalid(format!("user '{}' requires user and need", item.id));
            }
            resolve(&item.source_refs, &format!("user '{}'", item.id))?;
        }
        for item in &self.goals {
            check_id("goal", &item.id)?;
            if item.statement.trim().is_empty() {
                return invalid(format!("goal '{}' statement is required", item.id));
            }
            resolve(&item.source_refs, &format!("goal '{}'", item.id))?;
        }
        for item in &self.non_goals {
            check_id("non-goal", &item.id)?;
            if item.statement.trim().is_empty() {
                return invalid(format!("non-goal '{}' statement is required", item.id));
            }
            resolve(&item.source_refs, &format!("non-goal '{}'", item.id))?;
        }
        for item in &self.requirements {
            check_id("requirement", &item.id)?;
            if item.title.trim().is_empty() || item.statement.trim().is_empty() {
                return invalid(format!(
                    "requirement '{}' requires title and statement",
                    item.id
                ));
            }
            if item.acceptance_criteria.is_empty()
                || item
                    .acceptance_criteria
                    .iter()
                    .any(|criterion| criterion.trim().is_empty())
            {
                return invalid(format!(
                    "requirement '{}' requires at least one non-empty acceptance criterion",
                    item.id
                ));
            }
            if item.status == RequirementStatus::Confirmed && item.source_refs.is_empty() {
                return invalid(format!(
                    "confirmed requirement '{}' must link to source input",
                    item.id
                ));
            }
            resolve(&item.source_refs, &format!("requirement '{}'", item.id))?;
        }
        for item in &self.nfrs {
            check_id("nfr", &item.id)?;
            if item.statement.trim().is_empty() || item.category.trim().is_empty() {
                return invalid(format!("NFR '{}' requires statement and category", item.id));
            }
            resolve(&item.source_refs, &format!("NFR '{}'", item.id))?;
        }
        for item in &self.constraints {
            check_id("constraint", &item.id)?;
            if item.statement.trim().is_empty() {
                return invalid(format!("constraint '{}' statement is required", item.id));
            }
            resolve(&item.source_refs, &format!("constraint '{}'", item.id))?;
        }
        for item in &self.evidence {
            check_id("evidence", &item.id)?;
            if item.claim.trim().is_empty() || item.source_refs.is_empty() {
                return invalid(format!(
                    "evidence '{}' requires a claim and source_refs",
                    item.id
                ));
            }
            resolve(&item.source_refs, &format!("evidence '{}'", item.id))?;
        }
        for item in &self.assumptions {
            check_id("assumption", &item.id)?;
            if item.statement.trim().is_empty() || item.owner.trim().is_empty() {
                return invalid(format!(
                    "assumption '{}' requires statement and owner",
                    item.id
                ));
            }
            resolve(&item.source_refs, &format!("assumption '{}'", item.id))?;
        }
        for item in &self.contradictions {
            check_id("contradiction", &item.id)?;
            if item.statement.trim().is_empty() || item.owner.trim().is_empty() {
                return invalid(format!(
                    "contradiction '{}' requires statement and owner",
                    item.id
                ));
            }
            resolve(&item.source_refs, &format!("contradiction '{}'", item.id))?;
        }
        for item in &self.risks {
            check_id("risk", &item.id)?;
            if item.statement.trim().is_empty() || item.mitigation.trim().is_empty() {
                return invalid(format!(
                    "risk '{}' requires statement and mitigation",
                    item.id
                ));
            }
        }
        for item in &self.decisions {
            check_id("decision", &item.id)?;
            if item.question.trim().is_empty() || item.owner.trim().is_empty() {
                return invalid(format!(
                    "decision '{}' requires question and owner",
                    item.id
                ));
            }
            if item.status == DecisionStatus::Decided
                && item
                    .decision
                    .as_deref()
                    .is_none_or(|value| value.trim().is_empty())
            {
                return invalid(format!(
                    "decided decision '{}' requires a decision",
                    item.id
                ));
            }
            resolve(&item.source_refs, &format!("decision '{}'", item.id))?;
        }
        for item in &self.unknowns {
            check_id("unknown", &item.id)?;
            if item.question.trim().is_empty()
                || item.impact.trim().is_empty()
                || item.owner.trim().is_empty()
            {
                return invalid(format!(
                    "unknown '{}' requires question, impact and owner",
                    item.id
                ));
            }
            resolve(&item.source_refs, &format!("unknown '{}'", item.id))?;
        }
        Ok(())
    }

    /// Reject evidence/source references that are not backed by durable run
    /// inputs. `allowed` uses the stable reference forms documented on
    /// [`Source`]. This is the anti-fabrication check applied when a generated
    /// stage artifact becomes a durable packet.
    pub fn validate_known_sources(&self, allowed: &HashSet<String>) -> Result<(), PacketError> {
        let mut referenced = HashSet::new();
        for source in &self.sources {
            let expected_prefix = match source.kind {
                SourceKind::UserInput => "run:",
                SourceKind::QuestionAnswer => "answer:",
                SourceKind::StageArtifact => "artifact:",
                SourceKind::OwnerDecision => "run:",
                SourceKind::ExternalDocument => "external:",
            };
            if !source.reference.starts_with(expected_prefix) {
                return Err(PacketError::Invalid(format!(
                    "source '{}' kind does not match its reference '{}' (expected prefix '{}')",
                    source.id, source.reference, expected_prefix
                )));
            }
            if !allowed.contains(&source.reference) {
                return Err(PacketError::Invalid(format!(
                    "source '{}' cites '{}' which is not a durable input of this run; generated evidence cannot be fabricated",
                    source.id, source.reference
                )));
            }
            referenced.insert(source.id.as_str());
        }
        Ok(())
    }

    pub fn completeness(&self) -> Completeness {
        if !self.contradictions.is_empty() {
            return Completeness::Contradictory;
        }
        let open_decisions = self
            .decisions
            .iter()
            .any(|decision| decision.status == DecisionStatus::Open);
        let unresolved_requirements = self
            .requirements
            .iter()
            .any(|requirement| requirement.status != RequirementStatus::Confirmed);
        if open_decisions || unresolved_requirements || !self.unknowns.is_empty() {
            Completeness::Incomplete
        } else {
            Completeness::Complete
        }
    }

    /// Canonical content hash. Struct field order makes the serialized form
    /// deterministic for a given packet value.
    pub fn content_hash(&self) -> String {
        let bytes = serde_json::to_vec(self).unwrap_or_default();
        format!("{:x}", Sha256::digest(bytes))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PacketDocument {
    pub packet_id: String,
    pub revision: i64,
    pub content_hash: String,
    pub completeness: Completeness,
    pub packet: RequirementsPacket,
}

/// Portable JSON envelope. Markdown/JSON exports use the same envelope so the
/// approved revision, hash, provenance and review history always agree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PacketEnvelope {
    pub export_schema_version: u32,
    pub packet: PacketDocument,
    pub provenance: PacketProvenance,
    pub reviews: Vec<PacketReviewRecord>,
    pub history: Vec<PacketRevisionSummary>,
}

impl PacketEnvelope {
    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    pub fn to_markdown(&self) -> String {
        let packet = &self.packet.packet;
        let mut out = String::new();
        out.push_str(&format!("# {}\n\n", packet.title));
        out.push_str(&format!(
            "> Requirements packet v{} · `{}` revision {} · `{}` · **{}**\n\n",
            packet.schema_version,
            self.packet.packet_id,
            self.packet.revision,
            self.packet.content_hash,
            self.packet.completeness.as_str()
        ));
        out.push_str("## Portable packet metadata\n\n");
        out.push_str(&format!("- Packet ID: `{}`\n", self.packet.packet_id));
        out.push_str(&format!("- Revision: {}\n", self.packet.revision));
        out.push_str(&format!("- Content hash: `{}`\n", self.packet.content_hash));
        out.push_str(&format!(
            "- Completeness: **{}**\n",
            self.packet.completeness.as_str()
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

        out.push_str("\n## Problem\n\n");
        out.push_str(&packet.problem.statement);
        out.push_str("\n\n");

        section_list(&mut out, "Users", &packet.users, |item| {
            format!("**{}** — {}", item.user, item.need)
        });
        section_list(&mut out, "Goals", &packet.goals, |item| {
            item.statement.clone()
        });
        section_list(&mut out, "Non-goals", &packet.non_goals, |item| {
            item.statement.clone()
        });

        out.push_str("## Requirements\n\n");
        if packet.requirements.is_empty() {
            out.push_str("_No requirements recorded._\n\n");
        }
        for requirement in &packet.requirements {
            out.push_str(&format!(
                "### {} — {}\n\n{}\n\n- Kind: `{}`\n- Priority: `{}`\n- Status: **{}**\n",
                requirement.id,
                requirement.title,
                requirement.statement,
                serde_json::to_value(requirement.kind)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default(),
                serde_json::to_value(requirement.priority)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default(),
                serde_json::to_value(requirement.status)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default(),
            ));
            for criterion in &requirement.acceptance_criteria {
                out.push_str(&format!("- Acceptance: {}\n", criterion));
            }
            if !requirement.source_refs.is_empty() {
                out.push_str(&format!(
                    "- Sources: {}\n",
                    requirement.source_refs.join(", ")
                ));
            }
            out.push('\n');
        }

        section_list(
            &mut out,
            "Non-functional requirements",
            &packet.nfrs,
            |item| format!("{} ({})", item.statement, item.category),
        );
        section_list(&mut out, "Constraints", &packet.constraints, |item| {
            item.statement.clone()
        });
        section_list(&mut out, "Evidence", &packet.evidence, |item| {
            format!("{} — sources: {}", item.claim, item.source_refs.join(", "))
        });
        section_list(&mut out, "Assumptions", &packet.assumptions, |item| {
            format!("{} (owner: {})", item.statement, item.owner)
        });
        section_list(&mut out, "Contradictions", &packet.contradictions, |item| {
            format!("{} (owner: {})", item.statement, item.owner)
        });
        section_list(&mut out, "Risks", &packet.risks, |item| {
            format!(
                "{} — severity {}, likelihood {}, mitigation: {}",
                item.statement,
                serde_json::to_value(item.severity)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default(),
                serde_json::to_value(item.likelihood)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default(),
                item.mitigation
            )
        });
        section_list(&mut out, "Owned decisions", &packet.decisions, |item| {
            let status = if item.status == DecisionStatus::Decided {
                format!(
                    "decided: {}",
                    item.decision.as_deref().unwrap_or("(missing)")
                )
            } else {
                "OPEN".to_string()
            };
            format!("{} — {} (owner: {})", item.question, status, item.owner)
        });
        section_list(&mut out, "Unknowns", &packet.unknowns, |item| {
            format!(
                "{} — impact: {} (owner: {})",
                item.question, item.impact, item.owner
            )
        });

        out.push_str("## Sources\n\n");
        for source in &packet.sources {
            out.push_str(&format!(
                "- `{}` ({}) `{}`: {}\n",
                source.id,
                serde_json::to_value(source.kind)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default(),
                source.reference,
                source.excerpt
            ));
        }
        if packet.sources.is_empty() {
            out.push_str("_No sources recorded._\n");
        }

        out.push_str("\n## Approval and revision history\n\n");
        if self.history.is_empty() {
            out.push_str("_No revision history._\n");
        }
        for item in &self.history {
            out.push_str(&format!(
                "- r{} `{}` {} — {} by {} at {}{}\n",
                item.revision,
                item.revision_id,
                item.completeness.as_str(),
                item.content_hash,
                item.created_by,
                item.created_at,
                item.change_request
                    .as_ref()
                    .map(|reason| format!(" — change requested: {reason}"))
                    .unwrap_or_default()
            ));
        }
        if self.reviews.is_empty() {
            out.push_str("\n_No gate reviews recorded._\n");
        } else {
            for review in &self.reviews {
                out.push_str(&format!(
                    "- {} by {} at {} via gate `{}`{}{}\n",
                    review.outcome,
                    review.actor_name,
                    review.created_at,
                    review.gate_instance_id,
                    review
                        .reason
                        .as_ref()
                        .map(|reason| format!(" — {reason}"))
                        .unwrap_or_default(),
                    ""
                ));
            }
        }
        out
    }
}

fn section_list<T>(out: &mut String, title: &str, items: &[T], render: impl Fn(&T) -> String) {
    out.push_str(&format!("## {}\n\n", title));
    if items.is_empty() {
        out.push_str("_None recorded._\n\n");
        return;
    }
    for item in items {
        out.push_str(&format!("- {}\n", render(item)));
    }
    out.push('\n');
}

/// Simple line-level diff for review UIs and tests. The UI may render the same
/// data, but exports and server-side checks share one deterministic shape.
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
    const INCOMPLETE: &str = include_str!("../fixtures/incomplete.json");
    const CONTRADICTORY: &str = include_str!("../fixtures/contradictory.json");

    fn allowed() -> HashSet<String> {
        [
            "artifact:flow-a:stage-a",
            "artifact:flow-a:stage-b",
            "artifact:flow-a:artifact-a",
            "answer:q_example",
            "run:run_example",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    #[test]
    fn complete_fixture_is_valid_complete_and_hash_stable() {
        let packet = RequirementsPacket::from_json(COMPLETE).unwrap();
        packet.validate_known_sources(&allowed()).unwrap();
        assert_eq!(packet.completeness(), Completeness::Complete);
        assert_eq!(packet.content_hash(), packet.content_hash());
        assert_eq!(packet.content_hash().len(), 64);
        let changed = COMPLETE.replace("Cancel a booking", "Change a booking");
        let changed = RequirementsPacket::from_json(&changed).unwrap();
        assert_ne!(packet.content_hash(), changed.content_hash());
    }

    #[test]
    fn incomplete_and_contradictory_fixtures_are_honest_not_rejected_or_mislabelled() {
        let incomplete = RequirementsPacket::from_json(INCOMPLETE).unwrap();
        incomplete.validate_known_sources(&allowed()).unwrap();
        assert_eq!(incomplete.completeness(), Completeness::Incomplete);
        assert!(!incomplete.unknowns.is_empty());
        assert!(
            incomplete
                .decisions
                .iter()
                .any(|d| d.status == DecisionStatus::Open)
        );

        let contradictory = RequirementsPacket::from_json(CONTRADICTORY).unwrap();
        contradictory.validate_known_sources(&allowed()).unwrap();
        assert_eq!(contradictory.completeness(), Completeness::Contradictory);
    }

    #[test]
    fn confirmed_requirement_cannot_omit_or_invent_a_source() {
        let mut packet = RequirementsPacket::from_json(COMPLETE).unwrap();
        packet.requirements[0].source_refs.clear();
        let error = packet.validate().unwrap_err().to_string();
        assert!(error.contains("must link to source input"), "{error}");

        let mut packet = RequirementsPacket::from_json(COMPLETE).unwrap();
        packet.requirements[0].source_refs = vec!["SRC-INVENTED".into()];
        let error = packet.validate().unwrap_err().to_string();
        assert!(error.contains("invented evidence"), "{error}");

        let mut packet = RequirementsPacket::from_json(COMPLETE).unwrap();
        packet.sources[0].reference = "artifact:elsewhere:invented".into();
        let error = packet
            .validate_known_sources(&allowed())
            .unwrap_err()
            .to_string();
        assert!(error.contains("cannot be fabricated"), "{error}");
    }

    #[test]
    fn structurally_invalid_packets_are_rejected() {
        let mut packet = RequirementsPacket::from_json(COMPLETE).unwrap();
        packet.schema_version = 2;
        assert!(
            packet
                .validate()
                .unwrap_err()
                .to_string()
                .contains("schema_version")
        );

        let mut packet = RequirementsPacket::from_json(COMPLETE).unwrap();
        packet.requirements[0].acceptance_criteria.clear();
        assert!(
            packet
                .validate()
                .unwrap_err()
                .to_string()
                .contains("acceptance")
        );

        let mut packet = RequirementsPacket::from_json(COMPLETE).unwrap();
        packet.decisions.push(OwnedDecision {
            id: "DEC-1".into(),
            question: "Who owns rollout?".into(),
            owner: "product".into(),
            status: DecisionStatus::Open,
            decision: None,
            source_refs: vec![],
        });
        assert_eq!(packet.completeness(), Completeness::Incomplete);
    }

    #[test]
    fn markdown_and_json_exports_share_revision_hash_provenance_and_history() {
        let packet = RequirementsPacket::from_json(COMPLETE).unwrap();
        let envelope = PacketEnvelope {
            export_schema_version: 1,
            packet: PacketDocument {
                packet_id: "pkt_1".into(),
                revision: 2,
                content_hash: packet.content_hash(),
                completeness: packet.completeness(),
                packet: packet.clone(),
            },
            provenance: PacketProvenance {
                run_id: "run_1".into(),
                flow_name: Some("flow-a".into()),
                stage_name: Some("packet".into()),
                artifact_id: Some("art_1".into()),
                artifact_hash: Some("abc".into()),
                created_by: "system:packet-generation".into(),
                created_at: "2026-10-22T10:00:00Z".into(),
            },
            reviews: vec![PacketReviewRecord {
                id: "review_1".into(),
                gate_instance_id: "gate_1".into(),
                actor_id: "u1".into(),
                actor_name: "Reviewer".into(),
                outcome: "approved".into(),
                reason: None,
                created_at: "2026-10-22T10:05:00Z".into(),
            }],
            history: vec![PacketRevisionSummary {
                revision_id: "pr_2".into(),
                revision: 2,
                content_hash: packet.content_hash(),
                completeness: packet.completeness(),
                parent_revision_id: Some("pr_1".into()),
                change_request: Some("clarify cancellation".into()),
                created_by: "reviewer".into(),
                created_at: "2026-10-22T09:00:00Z".into(),
            }],
        };
        let markdown = envelope.to_markdown();
        assert!(markdown.contains("pkt_1"));
        assert!(markdown.contains(&packet.content_hash()));
        assert!(markdown.contains("Revision: 2"));
        assert!(markdown.contains("reviewer"));
        assert!(markdown.contains("approved by Reviewer"));
        assert!(markdown.contains("artifact:flow-a:stage-a"));
        let json = envelope.to_json_pretty();
        assert!(json.contains("\"content_hash\""));
        assert!(json.contains("\"history\""));
        assert!(json.contains("\"provenance\""));
    }

    #[test]
    fn line_diff_marks_changed_packet_sections() {
        let diff = diff_lines("Problem: A\nGoal: B\n", "Problem: A\nGoal: C\n");
        assert!(
            diff.iter()
                .any(|line| line.kind == DiffKind::Removed && line.text == "Goal: B")
        );
        assert!(
            diff.iter()
                .any(|line| line.kind == DiffKind::Added && line.text == "Goal: C")
        );
    }
}
