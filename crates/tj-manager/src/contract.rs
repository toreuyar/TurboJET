//! Immutable runtime contract resolved from a v2 execution bundle.
//!
//! This is the single source of runtime policy for new runs. Legacy v1 bundles
//! are handled only by `legacy_v1`, never through name/flow inference here.

use anyhow::{Context, Result, ensure};
use tj_config::bundle::{ExecutionBundle, SUPPORTED_BUNDLE_VERSIONS};
use tj_config::capability::{
    self, GateBindingKind, RequirementsPacketCapability, TaskPlanCapability,
    TaskSelectionCapability, VerificationCapability, WorkflowCapabilities, WorkspaceMode,
};
use tj_config::smg::StateMachineGraph;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Precondition {
    ApprovedPacket,
    ApprovedPlan,
    TaskSelection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityModule {
    Inputs,
    RequirementsPacket,
    TaskPlan,
    Publication,
    Verification,
    ExternalDelivery,
}

impl CapabilityModule {
    pub fn name(self) -> &'static str {
        match self {
            Self::Inputs => "inputs",
            Self::RequirementsPacket => "requirements_packet",
            Self::TaskPlan => "task_plan",
            Self::Publication => "publication",
            Self::Verification => "verification",
            Self::ExternalDelivery => "external_delivery",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RunContract {
    pub bundle_hash: String,
    pub workflow_id: Option<String>,
    pub graph: StateMachineGraph,
    pub capabilities: WorkflowCapabilities,
    pub capability_hash: String,
}

impl RunContract {
    /// Resolve and verify a v2 contract. This deliberately rejects v1 bundles:
    /// the isolated legacy adapter is the only v1 runtime path.
    pub fn from_bundle(bundle: &ExecutionBundle) -> Result<Self> {
        ensure!(
            SUPPORTED_BUNDLE_VERSIONS.contains(&bundle.schema_version),
            "unsupported execution bundle schema version {}",
            bundle.schema_version
        );
        ensure!(
            bundle.schema_version == 2,
            "run '{}' uses a v1 execution bundle; only the isolated legacy adapter may execute it",
            bundle.run_id
        );
        let capabilities = bundle
            .capability_contract
            .clone()
            .context("v2 execution bundle is missing its frozen capability contract")?;
        let capability_hash = bundle
            .capability_hash
            .clone()
            .context("v2 execution bundle is missing its capability hash")?;
        ensure!(
            capability::contract_hash(&capabilities)? == capability_hash,
            "execution bundle capability hash mismatch"
        );
        let effective = capability::effective_contract(&bundle.graph);
        ensure!(
            effective == capabilities,
            "frozen capability contract does not match the workflow graph declaration"
        );
        capabilities.validate(&bundle.graph)?;
        Ok(Self {
            bundle_hash: bundle.bundle_hash.clone(),
            workflow_id: bundle.workflow_id.clone(),
            graph: bundle.graph.clone(),
            capabilities,
            capability_hash,
        })
    }

    pub fn workspace_mode(&self) -> WorkspaceMode {
        self.capabilities.workspace_mode()
    }

    pub fn gate_binding(&self, gate: &str) -> GateBindingKind {
        self.capabilities.gate_binding(gate)
    }

    pub fn input_destination(&self) -> Option<&str> {
        self.capabilities.input_destination()
    }

    pub fn packet_producer(&self) -> Option<&RequirementsPacketCapability> {
        self.capabilities.requirements_packet.as_ref()
    }

    pub fn plan_producer(&self) -> Option<&TaskPlanCapability> {
        self.capabilities.task_plan.as_ref()
    }

    pub fn verification_for(&self, flow: &str) -> Option<&VerificationCapability> {
        self.capabilities.verification_for(flow)
    }

    pub fn publication_for(
        &self,
        flow: &str,
    ) -> Option<&tj_config::capability::PublicationCapability> {
        self.capabilities.publication_for(flow)
    }

    pub fn selection_after_gate(&self, gate: &str) -> Option<&TaskSelectionCapability> {
        self.capabilities.selection_after_gate(gate)
    }

    /// Flow that may start only after the declared task selection. Publication
    /// is the explicit target when it requires selection; otherwise it is the
    /// flow following the plan's binding gate in declared order.
    pub fn selection_target_flow(&self) -> Option<&str> {
        let plan = self.capabilities.task_plan.as_ref()?;
        if let Some(publication) = &self.capabilities.publication
            && publication.requires_task_selection
        {
            return Some(publication.flow.as_str());
        }
        let index = self
            .graph
            .flow_order
            .iter()
            .position(|candidate| candidate == &plan.binding_gate)?;
        self.graph
            .flow_order
            .iter()
            .skip(index + 1)
            .find(|candidate| {
                self.graph
                    .flows
                    .get(*candidate)
                    .is_some_and(|flow| flow.flow_type == tj_config::smg::FlowType::Flow)
            })
            .map(String::as_str)
    }

    pub fn external_delivery(&self) -> Option<&tj_config::capability::ExternalDeliveryCapability> {
        self.capabilities.external_delivery()
    }

    /// Every optional module declared by this contract, independent of names.
    pub fn declared_optional_modules(&self) -> Vec<CapabilityModule> {
        let mut modules = Vec::new();
        if self.capabilities.inputs.is_some() {
            modules.push(CapabilityModule::Inputs);
        }
        if self.capabilities.requirements_packet.is_some() {
            modules.push(CapabilityModule::RequirementsPacket);
        }
        if self.capabilities.task_plan.is_some() {
            modules.push(CapabilityModule::TaskPlan);
        }
        if self.capabilities.publication.is_some() {
            modules.push(CapabilityModule::Publication);
        }
        if !self.capabilities.verification.is_empty() {
            modules.push(CapabilityModule::Verification);
        }
        if self.capabilities.external_delivery.is_some() {
            modules.push(CapabilityModule::ExternalDelivery);
        }
        modules
    }

    /// Preconditions before an executable flow starts. These are data, never
    /// derived from a flow/stage name.
    pub fn preconditions_for(&self, flow: &str) -> Vec<Precondition> {
        let mut preconditions = Vec::new();
        if let Some(plan) = &self.capabilities.task_plan {
            // Validation requires a packet producer for every task plan in this
            // build, because the durable plan revision is packet-bound.
            if plan.produce_flow == flow {
                preconditions.push(Precondition::ApprovedPacket);
            }
        }
        if let Some(publication) = &self.capabilities.publication {
            if publication.flow == flow && publication.requires_task_selection {
                preconditions.push(Precondition::ApprovedPlan);
                preconditions.push(Precondition::TaskSelection);
            }
        }
        preconditions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tj_config::ConfigLoader;
    use tj_test_support::{DEFAULT_GATE, SyntheticTree, SyntheticWorkflow};

    #[test]
    fn contract_resolves_only_from_the_frozen_bundle() {
        let mut capabilities = serde_json::json!({
            "schema_version": 1,
            "workspace": {"kind": "none"},
            "gates": {}
        });
        capabilities["gates"][DEFAULT_GATE] = serde_json::json!("artifact");
        let tree = SyntheticTree::with_workflow(
            SyntheticWorkflow::new()
                .with_gate(DEFAULT_GATE)
                .with_capabilities(capabilities),
        );
        let loader = ConfigLoader::from_file(tree.config_path()).unwrap();
        let graph = loader.load_workflow_sources().unwrap().remove(0).graph;
        let bundle = ExecutionBundle::build(&loader, "run_contract", None, &graph).unwrap();
        let contract = RunContract::from_bundle(&bundle).unwrap();
        assert_eq!(contract.workspace_mode(), WorkspaceMode::None);
        assert_eq!(contract.gate_binding("gate-a"), GateBindingKind::Artifact);
        assert!(contract.declared_optional_modules().is_empty());
        assert!(contract.preconditions_for("flow-a").is_empty());
    }

    #[test]
    fn declared_modules_never_depend_on_flow_or_stage_names() {
        let graph: StateMachineGraph = serde_json::from_value(serde_json::json!({
            "version": 1,
            "name": "neutral-capabilities",
            "entry": "engineering",
            "exit": "deliver-code",
            "limits": {"maxIterations": 2, "maxSteps": 9, "maxDepth": 3},
            "flow-order": [
                "engineering",
                "review-gate",
                "shape",
                "shape-gate",
                "deliver-code"
            ],
            "flows": {
                "engineering": {"type": "flow", "entry": "make", "exit": "make", "stages": {
                    "make": {"agent": "agents/make/agent.json", "routes": {"success": "_next-flow"}}
                }},
                "review-gate": {"type": "gate", "routes": {"approved": "_next-flow", "rejected": "_rework"}, "stages": {}},
                "shape": {"type": "flow", "entry": "plan", "exit": "plan", "stages": {
                    "plan": {"agent": "agents/plan/agent.json", "routes": {"success": "_next-flow"}}
                }},
                "shape-gate": {"type": "gate", "routes": {"approved": "_next-flow", "rejected": "_rework"}, "stages": {}},
                "deliver-code": {"type": "flow", "entry": "compile-check", "exit": "sign-off", "stages": {
                    "compile-check": {"agent": "agents/compile/agent.json", "routes": {"success": "peer-review"}},
                    "peer-review": {"agent": "agents/peer/agent.json", "routes": {"success": "sign-off"}},
                    "sign-off": {"agent": "agents/sign/agent.json", "routes": {"success": "_exit"}}
                }}
            }
        }))
        .unwrap();
        let capabilities: WorkflowCapabilities = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "workspace": {"kind": "repository", "provisioning": "optional"},
            "gates": {"review-gate": "packet", "shape-gate": "plan"},
            "requirements_packet": {
                "produce_flow": "engineering", "producer_stage": "make",
                "binding_gate": "review-gate"
            },
            "task_plan": {
                "produce_flow": "shape", "validate_stage": "plan",
                "binding_gate": "shape-gate", "source_packet": true,
                "task_selection": {"after_gate": "shape-gate", "required": true}
            },
            "publication": {
                "flow": "deliver-code", "requires_task_selection": true,
                "required_evidence": [
                    {"role": "test", "stage": "compile-check", "execution": "shell", "criterion_mapping": true},
                    {"role": "review", "stage": "peer-review"},
                    {"role": "acceptance", "stage": "sign-off"}
                ]
            }
        }))
        .unwrap();
        capabilities.validate(&graph).unwrap();
        let contract = RunContract {
            bundle_hash: "bundle".into(),
            workflow_id: None,
            graph,
            capabilities,
            capability_hash: "hash".into(),
        };
        let runtime = crate::capabilities::CapabilityRuntime::new(&contract);
        let production = runtime.before_flow("engineering");
        assert!(!production.approved_packet && !production.task_selection);
        assert!(runtime.before_flow("shape").approved_packet);
        let delivery = runtime.before_flow("deliver-code");
        assert!(delivery.approved_plan);
        assert!(delivery.task_selection);
        assert!(runtime.after_flow("engineering").publication.is_none());
        assert!(runtime.after_flow("deliver-code").publication.is_some());
        assert_eq!(
            runtime.after_flow("deliver-code").publication.unwrap().flow,
            "deliver-code"
        );
        assert_eq!(
            runtime.after_gate("shape-gate"),
            Some("deliver-code"),
            "selection target is declaration data, not a flow name"
        );
        assert_eq!(
            contract.gate_binding("review-gate"),
            GateBindingKind::Packet
        );
        assert_eq!(contract.gate_binding("shape-gate"), GateBindingKind::Plan);
    }
}
