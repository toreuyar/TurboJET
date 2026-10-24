//! Deprecated legacy project-profile compatibility.
//!
//! This module is **not** a catalog or a source of workflow truth. New
//! project/workflow selection uses the registry (`catalog_id` / `workflow_id`).
//! These helpers remain only so already-frozen legacy projects and lanes keep
//! their historical execution policy until the isolated v1 adapter lands.
//!
//! There is deliberately no path table, no `for_smg_path` basename matching and
//! no list of templates. Adding a workflow must never require editing this
//! file.

use serde::{Deserialize, Serialize};

/// The supported repository-free Discovery profile.
pub const DISCOVERY_PROFILE: &str = "discovery";
/// The experimental full Discovery → Planning → Engineering profile.
pub const FULL_PROFILE: &str = "full";
/// Plan → act → check → result with bounded checker retry.
pub const AGENT_LOOP_PROFILE: &str = "agent-loop";
/// Clarify → produce → human review, with user clarification and revision.
pub const CLARIFY_PRODUCE_REVIEW_PROFILE: &str = "clarify-produce-review";
/// Research → synthesize → verify with local-source citation checks.
pub const RESEARCH_SYNTHESIZE_VERIFY_PROFILE: &str = "research-synthesize-verify";

/// How a terminal human gate identifies the exact reviewed output in the
/// legacy policy. New workflow contracts declare this in their capability
/// block (Batch 3); this enum remains for legacy/frozen behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateBinding {
    /// The immutable requirements packet revision (legacy Discovery/full SDLC).
    Packet,
    /// The immutable structured plan revision (legacy full SDLC planning).
    Plan,
    /// The latest immutable stage artifact of the reviewed flow (generic).
    Artifact,
    /// The graph has no human gate.
    None,
}

pub fn is_repository_free(id: &str) -> bool {
    matches!(
        id,
        DISCOVERY_PROFILE
            | AGENT_LOOP_PROFILE
            | CLARIFY_PRODUCE_REVIEW_PROFILE
            | RESEARCH_SYNTHESIZE_VERIFY_PROFILE
    )
}

pub fn is_generic(id: &str) -> bool {
    matches!(
        id,
        AGENT_LOOP_PROFILE | CLARIFY_PRODUCE_REVIEW_PROFILE | RESEARCH_SYNTHESIZE_VERIFY_PROFILE
    )
}

pub fn gate_binding(id: &str) -> GateBinding {
    match id {
        FULL_PROFILE | DISCOVERY_PROFILE => GateBinding::Packet,
        CLARIFY_PRODUCE_REVIEW_PROFILE => GateBinding::Artifact,
        AGENT_LOOP_PROFILE | RESEARCH_SYNTHESIZE_VERIFY_PROFILE => GateBinding::None,
        // Unknown legacy profiles keep the old full/unbound behavior; this is
        // never consulted for a catalog-registered workflow.
        _ => GateBinding::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_policy_is_explicit_and_has_no_catalog_table() {
        assert!(!is_repository_free(FULL_PROFILE));
        assert!(is_repository_free(DISCOVERY_PROFILE));
        assert!(is_repository_free(AGENT_LOOP_PROFILE));
        assert_eq!(
            gate_binding(CLARIFY_PRODUCE_REVIEW_PROFILE),
            GateBinding::Artifact
        );
        assert_eq!(gate_binding(AGENT_LOOP_PROFILE), GateBinding::None);
        assert!(is_generic(AGENT_LOOP_PROFILE));
        assert!(!is_generic(DISCOVERY_PROFILE));
    }
}
