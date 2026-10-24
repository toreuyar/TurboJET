//! Isolated compatibility adapter for already-frozen v1 execution bundles and
//! pre-bundle legacy runs.
//!
//! This module is the only engine location allowed to map historical business
//! profile/flow/gate names to behavior. It is never consulted for a schema-2
//! bundle or for a new run. The names exist solely to replay frozen work
//! exactly; they must not be copied into new capability modules.

use tj_config::capability::GateBindingKind;

/// Historical policy selected from the project profile recorded with a frozen
/// v1 run. Unknown profiles deliberately keep the old full/legacy behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyV1Policy {
    Discovery,
    Full,
    AgentLoop,
    ClarifyProduceReview,
    ResearchSynthesizeVerify,
    UnknownFull,
}

impl LegacyV1Policy {
    pub fn from_profile(profile: &str) -> Self {
        match profile {
            tj_config::profiles::DISCOVERY_PROFILE => Self::Discovery,
            tj_config::profiles::FULL_PROFILE => Self::Full,
            tj_config::profiles::AGENT_LOOP_PROFILE => Self::AgentLoop,
            tj_config::profiles::CLARIFY_PRODUCE_REVIEW_PROFILE => Self::ClarifyProduceReview,
            tj_config::profiles::RESEARCH_SYNTHESIZE_VERIFY_PROFILE => {
                Self::ResearchSynthesizeVerify
            }
            _ => Self::UnknownFull,
        }
    }

    pub fn default_profile() -> &'static str {
        tj_config::profiles::FULL_PROFILE
    }

    pub fn workspace_is_repository_free(self) -> bool {
        matches!(
            self,
            Self::Discovery
                | Self::AgentLoop
                | Self::ClarifyProduceReview
                | Self::ResearchSynthesizeVerify
        )
    }

    /// Frozen v1 gate behavior. `None` means the historical unbound gate that
    /// had no packet/plan/output binding at all.
    pub fn gate_binding(self, gate: &str, has_packet: bool) -> Option<GateBindingKind> {
        let profile = match self {
            Self::Discovery => tj_config::profiles::DISCOVERY_PROFILE,
            Self::Full => tj_config::profiles::FULL_PROFILE,
            Self::AgentLoop => tj_config::profiles::AGENT_LOOP_PROFILE,
            Self::ClarifyProduceReview => tj_config::profiles::CLARIFY_PRODUCE_REVIEW_PROFILE,
            Self::ResearchSynthesizeVerify => {
                tj_config::profiles::RESEARCH_SYNTHESIZE_VERIFY_PROFILE
            }
            Self::UnknownFull => "legacy-unknown",
        };
        match tj_config::profiles::gate_binding(profile) {
            tj_config::profiles::GateBinding::Artifact => Some(GateBindingKind::Artifact),
            tj_config::profiles::GateBinding::Packet => match gate {
                "discovery-gate" | "planning-gate" => Some(GateBindingKind::Packet),
                "review-gate" => Some(GateBindingKind::Artifact),
                _ if has_packet => Some(GateBindingKind::Packet),
                _ => None,
            },
            tj_config::profiles::GateBinding::Plan => Some(GateBindingKind::Plan),
            tj_config::profiles::GateBinding::None => None,
        }
    }

    /// The Discovery flow produced the requirements packet. This is a frozen
    /// v1 execution detail, not a rule for new workflows.
    pub fn produces_packet(self, flow: &str) -> bool {
        flow == tj_config::profiles::DISCOVERY_PROFILE
    }

    /// The Planning flow produced and deterministically validated a plan.
    pub fn validates_plan(self, flow: &str) -> bool {
        flow == "planning"
    }

    /// The research profile ran local citation consistency checks after its
    /// synthesis flow.
    pub fn verifies_local_citations(self, flow: &str) -> bool {
        self == Self::ResearchSynthesizeVerify
            && flow == tj_config::profiles::RESEARCH_SYNTHESIZE_VERIFY_PROFILE
    }

    /// Only the historical Engineering flow published, and only for profiles
    /// that use a repository workspace.
    pub fn publishes_on_flow(self, flow: &str) -> bool {
        !self.workspace_is_repository_free() && flow == "engineering"
    }

    /// The historical planning gate bound the structured plan in addition to
    /// the approved packet.
    pub fn requires_plan_at_gate(self, gate: &str) -> bool {
        gate == "planning-gate"
    }

    pub fn requires_approved_packet_before(self, flow: &str) -> bool {
        flow == "planning"
    }

    pub fn requires_task_selection_before(self, flow: &str) -> bool {
        !self.workspace_is_repository_free() && flow == "engineering"
    }

    pub fn task_selection_gate_state(self) -> &'static str {
        "engineering_task_selection_required"
    }

    pub fn task_selection_flow(self) -> &'static str {
        "engineering"
    }

    /// Frozen v1 publication declaration used only to drive the shared,
    /// parameterized publication module for historical Engineering runs. New
    /// workflows describe this in their own capability contract.
    pub fn publication_capability(
        self,
        flow: &str,
    ) -> tj_config::capability::PublicationCapability {
        use tj_config::capability::{
            EvidenceExecution, PublicationCapability, PublicationEvidence, PublicationMode,
            PublicationRole,
        };
        PublicationCapability {
            flow: flow.to_string(),
            mode: PublicationMode::DraftPr,
            requires_task_selection: true,
            required_evidence: vec![
                PublicationEvidence {
                    role: PublicationRole::Test,
                    stage: "test".into(),
                    execution: Some(EvidenceExecution::Shell),
                    criterion_mapping: true,
                },
                PublicationEvidence {
                    role: PublicationRole::Review,
                    stage: "review".into(),
                    execution: None,
                    criterion_mapping: false,
                },
                PublicationEvidence {
                    role: PublicationRole::Acceptance,
                    stage: "acceptance".into(),
                    execution: None,
                    criterion_mapping: false,
                },
            ],
        }
    }
}

/// Historical research-profile citation check. This is a frozen-v1 behavior
/// only: the flow is selected by `LegacyV1Policy::verifies_local_citations`,
/// never by this module inspecting a new workflow. The stage names below are
/// data from the historical graph, not a rule for arbitrary workflows.
pub fn validate_research_citations(
    workspace: &std::path::Path,
    artifacts: &std::collections::HashMap<String, serde_json::Value>,
) -> anyhow::Result<()> {
    let inputs = workspace.join("inputs");
    let canonical_inputs = std::fs::canonicalize(&inputs).map_err(|error| {
        anyhow::anyhow!(
            "supplied input directory is unavailable: {}: {error}",
            inputs.display()
        )
    })?;
    let citations = artifacts
        .get("synthesize")
        .ok_or_else(|| anyhow::anyhow!("research flow completed without a synthesize artifact"))?
        .get("citations")
        .and_then(|value| value.as_array())
        .ok_or_else(|| anyhow::anyhow!("synthesized answer has no citations array"))?;
    anyhow::ensure!(
        !citations.is_empty(),
        "synthesized answer has no source-backed citations"
    );
    let mut checked_groups = vec![("synthesize", citations)];
    if let Some(verify_citations) = artifacts
        .get("verify")
        .and_then(|value| value.get("citations"))
        .and_then(|value| value.as_array())
    {
        checked_groups.push(("verify", verify_citations));
    }
    for (group, citations) in checked_groups {
        for (index, citation) in citations.iter().enumerate() {
            let label = format!("{group} citation {index}");
            let path = citation
                .get("path")
                .and_then(|value| value.as_str())
                .ok_or_else(|| anyhow::anyhow!("{label} has no path"))?;
            let excerpt = citation
                .get("excerpt")
                .and_then(|value| value.as_str())
                .ok_or_else(|| anyhow::anyhow!("{label} has no excerpt"))?;
            anyhow::ensure!(
                !path.trim().is_empty() && !excerpt.trim().is_empty(),
                "{label} has an empty path or excerpt"
            );
            anyhow::ensure!(
                !path.contains("://") && !std::path::Path::new(path).is_absolute(),
                "{label} must be a relative local input path, not an external source"
            );
            let relative = std::path::Path::new(path);
            anyhow::ensure!(
                !relative
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir)),
                "{label} attempts to leave the input directory"
            );
            let candidate = workspace.join(relative);
            let canonical = std::fs::canonicalize(&candidate)
                .map_err(|error| anyhow::anyhow!("{label} path does not exist: {path}: {error}"))?;
            anyhow::ensure!(
                canonical.starts_with(&canonical_inputs),
                "{label} points outside inputs/: {path}"
            );
            let meta = std::fs::metadata(&canonical)?;
            anyhow::ensure!(
                meta.is_file() && meta.len() <= 2_000_000,
                "{label} is not a readable input file (or is too large): {path}"
            );
            let text = std::fs::read_to_string(&canonical).map_err(|error| {
                anyhow::anyhow!("{label} input is not readable text: {path}: {error}")
            })?;
            anyhow::ensure!(
                text.contains(excerpt),
                "{label} excerpt does not appear in the cited file: {path}"
            );
        }
    }
    Ok(())
}

/// Project workspace policy for pre-contract rows. New projects always carry an
/// explicit `workspace_kind` derived from the frozen workflow contract.
pub fn project_workspace_kind(project: &tj_db::Project) -> &str {
    project.workspace_kind.as_deref().unwrap_or_else(|| {
        if LegacyV1Policy::from_profile(&project.profile).workspace_is_repository_free() {
            "none"
        } else {
            "repository"
        }
    })
}
