//! Isolated legacy v1 adapter compatibility suite.
//!
//! This is the only default-code test allowed to name historical business
//! flows/gates. It constructs a real serialized v1 bundle in memory (no repo
//! assets) and proves that the compatibility adapter — and only it — maps
//! frozen names to old behavior. Run explicitly:
//!
//! `cargo test -p tj-manager --features legacy-compat-tests --test legacy_v1_adapter`

use std::collections::HashMap;

use serde_json::json;
use tj_config::bundle::{ExecutionBundle, hash_value};
use tj_config::smg::StateMachineGraph;
use tj_manager::legacy_v1::LegacyV1Policy;

fn v1_graph() -> StateMachineGraph {
    serde_json::from_value(json!({
        "version": 1,
        "name": "frozen legacy pipeline",
        "entry": "discovery",
        "exit": "engineering",
        "limits": {"maxIterations": 3, "maxSteps": 20, "maxDepth": 5},
        "flow-order": ["discovery", "discovery-gate", "planning", "planning-gate", "engineering"],
        "flows": {
            "discovery": {"type": "flow", "entry": "packet", "exit": "packet", "stages": {
                "packet": {"agent": "legacy/agents/discovery/packet/agent.json", "routes": {"success": "_next-flow"}}
            }},
            "discovery-gate": {"type": "gate", "routes": {"approved": "_next-flow", "rejected": "_rework"}, "stages": {}},
            "planning": {"type": "flow", "entry": "plan", "exit": "plan", "stages": {
                "plan": {"agent": "legacy/agents/planning/plan/agent.json", "routes": {"success": "_next-flow"}}
            }},
            "planning-gate": {"type": "gate", "routes": {"approved": "_next-flow", "rejected": "_rework"}, "stages": {}},
            "engineering": {"type": "flow", "entry": "code", "exit": "code", "stages": {
                "code": {"agent": "legacy/agents/engineering/code/agent.json", "routes": {"success": "_exit"}}
            }}
        }
    }))
    .unwrap()
}

fn v1_bundle() -> ExecutionBundle {
    let graph = v1_graph();
    let mut bundle = ExecutionBundle {
        schema_version: 1,
        run_id: "run_frozen_legacy".into(),
        workflow_id: None,
        workflow_hash: hash_value(&serde_json::to_value(&graph).unwrap()),
        graph,
        budgets: Default::default(),
        flows: HashMap::new(),
        bundle_hash: String::new(),
        created_at: "2026-10-22T00:00:00+00:00".into(),
        capability_contract: None,
        capability_hash: None,
    };
    bundle.bundle_hash = bundle.compute_hash().unwrap();
    bundle
}

#[test]
fn frozen_v1_bundle_round_trips_without_capability_fields() {
    let raw = serde_json::to_string(&v1_bundle()).unwrap();
    assert!(!raw.contains("capability_contract"));
    assert!(!raw.contains("capability_hash"));
    let parsed = ExecutionBundle::from_json(&raw).unwrap();
    assert_eq!(parsed.schema_version, 1);
}

#[test]
fn adapter_maps_only_historical_frozen_behavior() {
    let full = LegacyV1Policy::from_profile("full");
    assert!(!full.workspace_is_repository_free());
    assert_eq!(
        full.gate_binding("discovery-gate", false),
        Some(tj_config::capability::GateBindingKind::Packet)
    );
    assert_eq!(
        full.gate_binding("planning-gate", false),
        Some(tj_config::capability::GateBindingKind::Packet)
    );
    assert_eq!(
        full.gate_binding("review-gate", false),
        Some(tj_config::capability::GateBindingKind::Artifact)
    );
    assert_eq!(
        full.gate_binding("custom-gate", true),
        Some(tj_config::capability::GateBindingKind::Packet)
    );
    assert_eq!(full.gate_binding("custom-gate", false), None);
    assert!(full.produces_packet("discovery"));
    assert!(full.validates_plan("planning"));
    assert!(full.publishes_on_flow("engineering"));
    assert!(full.requires_task_selection_before("engineering"));
    assert!(full.requires_approved_packet_before("planning"));
    assert!(full.requires_plan_at_gate("planning-gate"));
    assert!(!full.publishes_on_flow("discovery"));

    let discovery = LegacyV1Policy::from_profile("discovery");
    assert!(discovery.workspace_is_repository_free());
    assert_eq!(
        discovery.gate_binding("discovery-gate", false),
        Some(tj_config::capability::GateBindingKind::Packet)
    );
    assert!(!discovery.publishes_on_flow("engineering"));

    let research = LegacyV1Policy::from_profile("research-synthesize-verify");
    assert!(research.workspace_is_repository_free());
    assert!(research.verifies_local_citations("research-synthesize-verify"));
    assert!(!research.verifies_local_citations("discovery"));

    let unknown = LegacyV1Policy::from_profile("not-a-profile");
    assert_eq!(unknown, LegacyV1Policy::UnknownFull);
    assert!(!unknown.workspace_is_repository_free());
    assert_eq!(
        LegacyV1Policy::default_profile(),
        tj_config::profiles::FULL_PROFILE
    );
}
