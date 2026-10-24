//! Versioned, declarative workflow capabilities.
//!
//! Capabilities are embedded in the workflow document, so they travel with the
//! content hash and are frozen into the execution bundle. The engine never
//! infers a capability from a flow, stage, gate, catalog/profile or file name:
//! it reads this declaration (or the explicit generic default) and nothing else.

use crate::smg::{FlowType, StateMachineGraph};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::{Component, Path, PathBuf};

/// The only capability schema version this engine can admit for new sources.
pub const CAPABILITY_SCHEMA_VERSION: u32 = 1;

/// The optional `capabilities` block on a workflow graph.
///
/// `schema_version` is required whenever the block is present. Unknown fields,
/// module names, versions and kinds fail deserialization/validation before any
/// registry row or execution effect exists.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowCapabilities {
    pub schema_version: u32,
    /// Generic default: an isolated non-Git run workspace.
    #[serde(default)]
    pub workspace: WorkspaceCapability,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs: Option<InputsCapability>,
    /// Gate flow name -> immutable review identity kind. Omitted gates default
    /// to `artifact`; `none` is never a valid new declaration.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub gates: BTreeMap<String, GateBindingKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requirements_packet: Option<RequirementsPacketCapability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_plan: Option<TaskPlanCapability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication: Option<PublicationCapability>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verification: Vec<VerificationCapability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_delivery: Option<ExternalDeliveryCapability>,
}

impl Default for WorkflowCapabilities {
    fn default() -> Self {
        Self {
            schema_version: CAPABILITY_SCHEMA_VERSION,
            workspace: WorkspaceCapability::default(),
            inputs: None,
            gates: BTreeMap::new(),
            requirements_packet: None,
            task_plan: None,
            publication: None,
            verification: Vec::new(),
            external_delivery: None,
        }
    }
}

impl WorkflowCapabilities {
    /// The explicit generic contract used when a graph has no `capabilities`
    /// block. This is a product default, not business inference.
    pub fn generic() -> Self {
        Self::default()
    }

    pub fn validate(&self, graph: &StateMachineGraph) -> Result<()> {
        ensure!(
            self.schema_version == CAPABILITY_SCHEMA_VERSION,
            "workflow '{}': unsupported capabilities.schema_version {} (supported: {})",
            graph.name,
            self.schema_version,
            CAPABILITY_SCHEMA_VERSION
        );
        self.workspace.validate(graph)?;
        if let Some(inputs) = &self.inputs {
            inputs.validate(graph)?;
        }
        self.validate_gates(graph)?;
        if let Some(packet) = &self.requirements_packet {
            packet.validate(graph, self)?;
        }
        if let Some(plan) = &self.task_plan {
            plan.validate(graph, self)?;
        }
        if let Some(publication) = &self.publication {
            publication.validate(graph, self)?;
        }
        let mut verified_flows = HashSet::new();
        for verification in &self.verification {
            ensure!(
                verified_flows.insert(verification.flow.as_str()),
                "workflow '{}': capabilities.verification declares flow '{}' more than once; only one verification policy per flow is executed",
                graph.name,
                verification.flow
            );
            verification.validate(graph)?;
        }
        if let Some(delivery) = &self.external_delivery {
            delivery.validate(graph)?;
        }
        Ok(())
    }

    pub fn workspace_mode(&self) -> WorkspaceMode {
        self.workspace.kind
    }

    /// Declared gate binding, or the generic safe default (`artifact`).
    pub fn gate_binding(&self, gate: &str) -> GateBindingKind {
        self.gates
            .get(gate)
            .copied()
            .unwrap_or(GateBindingKind::Artifact)
    }

    pub fn input_destination(&self) -> Option<&str> {
        self.inputs
            .as_ref()
            .map(|inputs| inputs.copy_project_inputs_to.as_str())
    }

    pub fn publication_for(&self, flow: &str) -> Option<&PublicationCapability> {
        self.publication
            .as_ref()
            .filter(|publication| publication.flow == flow)
    }

    pub fn verification_for(&self, flow: &str) -> Option<&VerificationCapability> {
        self.verification
            .iter()
            .find(|verification| verification.flow == flow)
    }

    pub fn selection_after_gate(&self, gate: &str) -> Option<&TaskSelectionCapability> {
        self.task_plan
            .as_ref()
            .and_then(|plan| plan.task_selection.as_ref())
            .filter(|selection| selection.required && selection.after_gate == gate)
    }

    pub fn external_delivery(&self) -> Option<&ExternalDeliveryCapability> {
        self.external_delivery.as_ref()
    }

    fn validate_gates(&self, graph: &StateMachineGraph) -> Result<()> {
        for (gate, binding) in &self.gates {
            ensure!(
                graph
                    .flows
                    .get(gate)
                    .is_some_and(|flow| flow.flow_type == FlowType::Gate),
                "workflow '{}': capabilities.gates references '{}' but no gate flow with that name exists",
                graph.name,
                gate
            );
            match binding {
                GateBindingKind::Artifact => {}
                GateBindingKind::Packet => {
                    let packet = self.requirements_packet.as_ref().with_context(|| {
                        format!(
                            "workflow '{}': gate '{}' binds a requirements packet but capabilities.requirements_packet is not declared",
                            graph.name, gate
                        )
                    })?;
                    ensure!(
                        packet.binding_gate == *gate,
                        "workflow '{}': gate '{}' is packet-bound but requirements_packet.binding_gate is '{}'",
                        graph.name,
                        gate,
                        packet.binding_gate
                    );
                }
                GateBindingKind::Plan => {
                    let plan = self.task_plan.as_ref().with_context(|| {
                        format!(
                            "workflow '{}': gate '{}' binds a task plan but capabilities.task_plan is not declared",
                            graph.name, gate
                        )
                    })?;
                    ensure!(
                        plan.binding_gate == *gate,
                        "workflow '{}': gate '{}' is plan-bound but task_plan.binding_gate is '{}'",
                        graph.name,
                        gate,
                        plan.binding_gate
                    );
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GateBindingKind {
    #[default]
    Artifact,
    Packet,
    Plan,
}

impl GateBindingKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Artifact => "artifact",
            Self::Packet => "packet",
            Self::Plan => "plan",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceMode {
    #[default]
    None,
    Repository,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryProvisioning {
    Optional,
    Required,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceCapability {
    #[serde(default)]
    pub kind: WorkspaceMode,
    /// Required for `repository`; forbidden for `none`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provisioning: Option<RepositoryProvisioning>,
}

impl WorkspaceCapability {
    fn validate(&self, graph: &StateMachineGraph) -> Result<()> {
        match self.kind {
            WorkspaceMode::None => ensure!(
                self.provisioning.is_none(),
                "workflow '{}': capabilities.workspace.kind 'none' must not declare provisioning",
                graph.name
            ),
            WorkspaceMode::Repository => ensure!(
                self.provisioning.is_some(),
                "workflow '{}': capabilities.workspace.kind 'repository' must declare provisioning ('optional' or 'required')",
                graph.name
            ),
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InputsCapability {
    /// Safe workspace-relative destination for copied project inputs.
    pub copy_project_inputs_to: String,
}

impl InputsCapability {
    fn validate(&self, graph: &StateMachineGraph) -> Result<()> {
        validate_safe_relative_path(
            &format!(
                "workflow '{}': capabilities.inputs.copy_project_inputs_to",
                graph.name
            ),
            &self.copy_project_inputs_to,
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RequirementsPacketCapability {
    pub produce_flow: String,
    pub producer_stage: String,
    pub binding_gate: String,
}

impl RequirementsPacketCapability {
    fn validate(&self, graph: &StateMachineGraph, caps: &WorkflowCapabilities) -> Result<()> {
        let produce_index = executable_flow_index(
            graph,
            "capabilities.requirements_packet.produce_flow",
            &self.produce_flow,
        )?;
        let gate_index = gate_index(
            graph,
            "capabilities.requirements_packet.binding_gate",
            &self.binding_gate,
        )?;
        ensure!(
            produce_index < gate_index,
            "workflow '{}': requirements_packet.produce_flow '{}' must run before binding_gate '{}'",
            graph.name,
            self.produce_flow,
            self.binding_gate
        );
        ensure!(
            graph.flows[&self.produce_flow]
                .stages
                .contains_key(&self.producer_stage),
            "workflow '{}': requirements_packet.producer_stage '{}' does not exist in flow '{}'",
            graph.name,
            self.producer_stage,
            self.produce_flow
        );
        ensure!(
            caps.gates.get(&self.binding_gate) == Some(&GateBindingKind::Packet),
            "workflow '{}': requirements_packet.binding_gate '{}' must declare the packet gate binding",
            graph.name,
            self.binding_gate
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskPlanCapability {
    pub produce_flow: String,
    pub validate_stage: String,
    pub binding_gate: String,
    #[serde(default)]
    pub source_packet: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_selection: Option<TaskSelectionCapability>,
}

impl TaskPlanCapability {
    fn validate(&self, graph: &StateMachineGraph, caps: &WorkflowCapabilities) -> Result<()> {
        let produce_index = executable_flow_index(
            graph,
            "capabilities.task_plan.produce_flow",
            &self.produce_flow,
        )?;
        let binding_gate_index = gate_index(
            graph,
            "capabilities.task_plan.binding_gate",
            &self.binding_gate,
        )?;
        ensure!(
            produce_index < binding_gate_index,
            "workflow '{}': task_plan.produce_flow '{}' must run before binding_gate '{}'",
            graph.name,
            self.produce_flow,
            self.binding_gate
        );
        // This build's durable plan revision is bound to one approved packet
        // revision. Declaring a task plan without a packet producer would be
        // accepted by a loose schema and then fail only after a model call, so
        // reject the inconsistent module dependency before any effect.
        let packet = caps.requirements_packet.as_ref().context(format!(
            "workflow '{}': task_plan requires capabilities.requirements_packet because every durable plan revision is bound to one approved packet identity",
            graph.name
        ))?;
        let packet_gate_index = gate_index(
            graph,
            "capabilities.requirements_packet.binding_gate",
            &packet.binding_gate,
        )?;
        ensure!(
            packet_gate_index < produce_index,
            "workflow '{}': task_plan.produce_flow '{}' must run after the approved packet gate '{}'",
            graph.name,
            self.produce_flow,
            packet.binding_gate
        );
        ensure!(
            graph.flows[&self.produce_flow]
                .stages
                .contains_key(&self.validate_stage),
            "workflow '{}': task_plan.validate_stage '{}' does not exist in flow '{}'",
            graph.name,
            self.validate_stage,
            self.produce_flow
        );
        ensure!(
            caps.gates.get(&self.binding_gate) == Some(&GateBindingKind::Plan),
            "workflow '{}': task_plan.binding_gate '{}' must declare the plan gate binding",
            graph.name,
            self.binding_gate
        );
        if let Some(selection) = &self.task_selection {
            ensure!(
                selection.after_gate == self.binding_gate,
                "workflow '{}': task_plan.task_selection.after_gate '{}' must equal task_plan.binding_gate '{}'",
                graph.name,
                selection.after_gate,
                self.binding_gate
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskSelectionCapability {
    pub after_gate: String,
    pub required: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PublicationMode {
    #[default]
    DraftPr,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PublicationRole {
    Test,
    Review,
    Acceptance,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceExecution {
    Shell,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PublicationEvidence {
    pub role: PublicationRole,
    pub stage: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<EvidenceExecution>,
    #[serde(default)]
    pub criterion_mapping: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PublicationCapability {
    pub flow: String,
    #[serde(default)]
    pub mode: PublicationMode,
    #[serde(default)]
    pub requires_task_selection: bool,
    pub required_evidence: Vec<PublicationEvidence>,
}

impl PublicationCapability {
    fn validate(&self, graph: &StateMachineGraph, caps: &WorkflowCapabilities) -> Result<()> {
        let flow_index = executable_flow_index(graph, "capabilities.publication.flow", &self.flow)?;
        ensure!(
            caps.workspace.kind == WorkspaceMode::Repository,
            "workflow '{}': publication requires capabilities.workspace.kind 'repository'",
            graph.name
        );
        ensure!(
            !self.required_evidence.is_empty() && self.required_evidence.len() <= 3,
            "workflow '{}': publication.required_evidence must declare 1–3 entries",
            graph.name
        );
        let mut roles = HashSet::new();
        for evidence in &self.required_evidence {
            ensure!(
                roles.insert(evidence.role),
                "workflow '{}': publication.required_evidence repeats role '{:?}'",
                graph.name,
                evidence.role
            );
            ensure!(
                graph.flows[&self.flow].stages.contains_key(&evidence.stage),
                "workflow '{}': publication evidence stage '{}' does not exist in flow '{}'",
                graph.name,
                evidence.stage,
                self.flow
            );
        }
        if self.requires_task_selection {
            let plan = caps.task_plan.as_ref().context(format!(
                "workflow '{}': publication.requires_task_selection is true but task_plan is not declared",
                graph.name
            ))?;
            let selection = plan.task_selection.as_ref().context(format!(
                "workflow '{}': publication.requires_task_selection requires task_plan.task_selection with required: true",
                graph.name
            ))?;
            ensure!(
                selection.required,
                "workflow '{}': publication.requires_task_selection requires task_plan.task_selection.required: true",
                graph.name
            );
            let gate_index = gate_index(
                graph,
                "capabilities.task_plan.binding_gate",
                &plan.binding_gate,
            )?;
            ensure!(
                gate_index < flow_index,
                "workflow '{}': publication.flow '{}' must run after the task-selection gate '{}'",
                graph.name,
                self.flow,
                plan.binding_gate
            );
        }
        // Plan-less publication is supported for evidence that stands on its
        // own. A criterion mapping, however, needs the approved task's exact
        // acceptance criteria; silently accepting an empty mapping would claim
        // a feature the run cannot enforce.
        if self
            .required_evidence
            .iter()
            .any(|evidence| evidence.criterion_mapping)
        {
            let plan = caps.task_plan.as_ref().context(format!(
                "workflow '{}': publication evidence with criterion_mapping requires a task_plan declaration with a required approved task selection",
                graph.name
            ))?;
            let selection = plan.task_selection.as_ref().context(format!(
                "workflow '{}': publication criterion_mapping requires task_plan.task_selection.required: true so approved acceptance criteria exist",
                graph.name
            ))?;
            ensure!(
                selection.required,
                "workflow '{}': publication criterion_mapping requires task_plan.task_selection.required: true",
                graph.name
            );
            let gate_index = gate_index(
                graph,
                "capabilities.task_plan.binding_gate",
                &plan.binding_gate,
            )?;
            ensure!(
                gate_index < flow_index,
                "workflow '{}': publication.flow '{}' must run after the selection gate '{}' because its evidence maps to approved task criteria",
                graph.name,
                self.flow,
                plan.binding_gate
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationKind {
    LocalCitations,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VerificationCapability {
    pub kind: VerificationKind,
    pub flow: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer_stage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs_dir: Option<String>,
}

impl VerificationCapability {
    fn validate(&self, graph: &StateMachineGraph) -> Result<()> {
        executable_flow_index(graph, "capabilities.verification.flow", &self.flow)?;
        if let Some(stage) = &self.producer_stage {
            ensure!(
                graph.flows[&self.flow].stages.contains_key(stage),
                "workflow '{}': verification.producer_stage '{}' does not exist in flow '{}'",
                graph.name,
                stage,
                self.flow
            );
        }
        if let Some(inputs_dir) = &self.inputs_dir {
            validate_safe_relative_path(
                &format!(
                    "workflow '{}': capabilities.verification.inputs_dir",
                    graph.name
                ),
                inputs_dir,
            )?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExternalDeliveryCapability {
    pub after_flow: String,
    /// Automatic dispatch is never supported; this must be explicitly true.
    pub explicit_request_only: bool,
}

impl ExternalDeliveryCapability {
    fn validate(&self, graph: &StateMachineGraph) -> Result<()> {
        executable_flow_index(
            graph,
            "capabilities.external_delivery.after_flow",
            &self.after_flow,
        )?;
        ensure!(
            self.explicit_request_only,
            "workflow '{}': external_delivery.explicit_request_only must be true; automatic delivery dispatch is unsupported",
            graph.name
        );
        Ok(())
    }
}

/// The effective contract for a graph: the explicit block, or the generic
/// default when it is absent.
pub fn effective_contract(graph: &StateMachineGraph) -> WorkflowCapabilities {
    graph
        .capabilities
        .clone()
        .unwrap_or_else(WorkflowCapabilities::generic)
}

/// Validate the optional capability declaration before registry admission,
/// project/lane selection or bundle construction.
pub fn validate_for_graph(graph: &StateMachineGraph) -> Result<()> {
    if let Some(capabilities) = &graph.capabilities {
        capabilities.validate(graph)?;
    }
    Ok(())
}

/// Canonical hash of the frozen contract. Field order is irrelevant because the
/// contract is converted to a `serde_json::Value` (sorted object keys).
pub fn contract_hash(capabilities: &WorkflowCapabilities) -> Result<String> {
    let value = serde_json::to_value(capabilities).context("serialize capability contract")?;
    Ok(crate::bundle::hash_value(&value))
}

/// Lexically safe workspace-relative path. Runtime modules must additionally
/// reject symlink escapes when they resolve the path against a real workspace.
pub fn validate_safe_relative_path(label: &str, value: &str) -> Result<()> {
    ensure!(!value.trim().is_empty(), "{label} must not be empty");
    ensure!(
        !value.contains('\\') && !value.contains('\0'),
        "{label} must use a platform-neutral relative path"
    );
    let path = Path::new(value);
    ensure!(!path.is_absolute(), "{label} must be relative");
    let mut components = 0usize;
    for component in path.components() {
        match component {
            Component::Normal(_) => components += 1,
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                bail!("{label} must not contain '.', '..', roots or prefixes: {value}")
            }
        }
    }
    ensure!(components > 0, "{label} must name a path");
    Ok(())
}

/// Resolve a validated relative path and reject escapes through symlinks or
/// `..` after canonicalization. Returns the joined path (which may not exist
/// yet when used to create a destination directory).
pub fn resolve_within_workspace(base: &Path, relative: &str) -> Result<PathBuf> {
    validate_safe_relative_path("workspace-relative path", relative)?;
    std::fs::create_dir_all(base)
        .with_context(|| format!("cannot create workspace base {}", base.display()))?;
    let canonical_base = std::fs::canonicalize(base)
        .with_context(|| format!("cannot canonicalize workspace base {}", base.display()))?;
    let joined = canonical_base.join(relative);
    // Resolve the nearest existing ancestor so a not-yet-created destination
    // cannot hide an escaping symlink in its parent chain.
    let mut ancestor = joined.as_path();
    while !ancestor.exists() {
        ancestor = ancestor.parent().with_context(|| {
            format!(
                "cannot resolve a parent for workspace destination {}",
                joined.display()
            )
        })?;
    }
    let canonical_ancestor = std::fs::canonicalize(ancestor)
        .with_context(|| format!("cannot canonicalize {}", ancestor.display()))?;
    ensure!(
        canonical_ancestor.starts_with(&canonical_base),
        "workspace path escapes the run workspace: {}",
        joined.display()
    );
    Ok(joined)
}

fn executable_flow_index(graph: &StateMachineGraph, label: &str, flow: &str) -> Result<usize> {
    let index = graph
        .flow_order
        .iter()
        .position(|candidate| candidate == flow)
        .with_context(|| {
            format!(
                "workflow '{}': {label} references '{}' which is not in flow-order",
                graph.name, flow
            )
        })?;
    ensure!(
        graph
            .flows
            .get(flow)
            .is_some_and(|f| f.flow_type == FlowType::Flow),
        "workflow '{}': {label} must reference an executable flow, but '{}' is a gate",
        graph.name,
        flow
    );
    Ok(index)
}

fn gate_index(graph: &StateMachineGraph, label: &str, gate: &str) -> Result<usize> {
    let index = graph
        .flow_order
        .iter()
        .position(|candidate| candidate == gate)
        .with_context(|| {
            format!(
                "workflow '{}': {label} references '{}' which is not in flow-order",
                graph.name, gate
            )
        })?;
    ensure!(
        graph
            .flows
            .get(gate)
            .is_some_and(|f| f.flow_type == FlowType::Gate),
        "workflow '{}': {label} must reference a gate flow, but '{}' is not a gate",
        graph.name,
        gate
    );
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn graph() -> StateMachineGraph {
        serde_json::from_value(json!({
            "version": 1,
            "name": "any-review",
            "entry": "collect",
            "exit": "publish",
            "limits": {"maxIterations": 2, "maxSteps": 9, "maxDepth": 3},
            "flow-order": ["collect", "approve", "publish"],
            "flows": {
                "collect": {"type": "flow", "entry": "gather", "exit": "write", "stages": {
                    "gather": {"agent": "config/stages/gather/agent.json", "routes": {"success": "write"}},
                    "write": {"agent": "config/stages/write/agent.json", "routes": {"success": "_next-flow"}}
                }},
                "approve": {"type": "gate", "routes": {"approved": "_next-flow", "rejected": "_rework"}, "stages": {}},
                "publish": {"type": "flow", "entry": "ship", "exit": "ship", "stages": {
                    "ship": {"agent": "config/stages/ship/agent.json", "routes": {"success": "_exit"}}
                }}
            }
        }))
        .unwrap()
    }

    fn full_graph() -> StateMachineGraph {
        serde_json::from_value(json!({
            "version": 1,
            "name": "any-delivery",
            "entry": "collect",
            "exit": "publish",
            "limits": {"maxIterations": 2, "maxSteps": 9, "maxDepth": 3},
            "flow-order": ["collect", "packet-approval", "prepare", "plan-approval", "publish"],
            "flows": {
                "collect": {"type": "flow", "entry": "gather", "exit": "write", "stages": {
                    "gather": {"agent": "config/stages/gather/agent.json", "routes": {"success": "write"}},
                    "write": {"agent": "config/stages/write/agent.json", "routes": {"success": "_next-flow"}}
                }},
                "packet-approval": {"type": "gate", "routes": {"approved": "_next-flow", "rejected": "_rework"}, "stages": {}},
                "prepare": {"type": "flow", "entry": "plan", "exit": "plan", "stages": {
                    "plan": {"agent": "config/stages/plan/agent.json", "routes": {"success": "_next-flow"}}
                }},
                "plan-approval": {"type": "gate", "routes": {"approved": "_next-flow", "rejected": "_rework"}, "stages": {}},
                "publish": {"type": "flow", "entry": "ship", "exit": "ship", "stages": {
                    "ship": {"agent": "config/stages/ship/agent.json", "routes": {"success": "_exit"}}
                }}
            }
        }))
        .unwrap()
    }

    fn full() -> WorkflowCapabilities {
        serde_json::from_value(json!({
            "schema_version": 1,
            "workspace": {"kind": "repository", "provisioning": "optional"},
            "inputs": {"copy_project_inputs_to": "inputs"},
            "gates": {"packet-approval": "packet", "plan-approval": "plan"},
            "requirements_packet": {
                "produce_flow": "collect", "producer_stage": "write", "binding_gate": "packet-approval"
            },
            "task_plan": {
                "produce_flow": "prepare", "validate_stage": "plan", "binding_gate": "plan-approval",
                "source_packet": true,
                "task_selection": {"after_gate": "plan-approval", "required": true}
            },
            "publication": {
                "flow": "publish", "mode": "draft_pr", "requires_task_selection": true,
                "required_evidence": [
                    {"role": "test", "stage": "ship", "execution": "shell", "criterion_mapping": true},
                    {"role": "review", "stage": "ship"},
                    {"role": "acceptance", "stage": "ship"}
                ]
            },
            "verification": [
                {"kind": "local_citations", "flow": "collect", "producer_stage": "write", "inputs_dir": "inputs"}
            ],
            "external_delivery": {"after_flow": "publish", "explicit_request_only": true}
        }))
        .unwrap()
    }

    #[test]
    fn absent_capabilities_use_the_explicit_generic_default() {
        let graph = graph();
        let contract = effective_contract(&graph);
        assert_eq!(contract, WorkflowCapabilities::generic());
        assert_eq!(contract.schema_version, 1);
        assert_eq!(contract.workspace.kind, WorkspaceMode::None);
        assert_eq!(contract.gate_binding("approve"), GateBindingKind::Artifact);
        contract.validate(&graph).unwrap();
        assert_eq!(
            contract_hash(&contract).unwrap(),
            contract_hash(&WorkflowCapabilities::generic()).unwrap()
        );
    }

    #[test]
    fn complete_declaration_validates_for_arbitrary_flow_names() {
        let graph = full_graph();
        let contract = full();
        contract.validate(&graph).unwrap();
        assert_eq!(
            contract.gate_binding("packet-approval"),
            GateBindingKind::Packet
        );
        assert_eq!(
            contract.gate_binding("plan-approval"),
            GateBindingKind::Plan
        );
        assert_eq!(
            contract.gate_binding("other-gate"),
            GateBindingKind::Artifact
        );
        assert_eq!(contract.workspace.kind, WorkspaceMode::Repository);
        assert!(contract.publication_for("publish").is_some());
        assert!(contract.publication_for("collect").is_none());
    }

    #[test]
    fn unknown_versions_fields_and_kinds_fail_before_use() {
        for raw in [
            json!({"schema_version": 0}),
            json!({"schema_version": 2}),
            json!({"schema_version": 1, "unknown_module": {}}),
            json!({"schema_version": 1, "workspace": {"kind": "container"}}),
            json!({"schema_version": 1, "gates": {"packet-approval": "none"}}),
            json!({"schema_version": 1, "publication": {"flow": "publish", "mode": "merge", "required_evidence": []}}),
            json!({"schema_version": 1, "verification": [{"kind": "source_trust", "flow": "collect"}]}),
        ] {
            match serde_json::from_value::<WorkflowCapabilities>(raw.clone()) {
                Ok(contract) => {
                    assert!(
                        contract.validate(&full_graph()).is_err(),
                        "must reject {raw}"
                    );
                }
                Err(_) => {}
            }
        }
    }

    #[test]
    fn unknown_references_and_unsafe_paths_fail_validation() {
        let graph = full_graph();
        let mut contract = full();
        contract.requirements_packet.as_mut().unwrap().binding_gate = "missing".into();
        assert!(
            contract
                .validate(&graph)
                .unwrap_err()
                .to_string()
                .contains("missing")
        );

        let mut contract = full();
        contract.inputs.as_mut().unwrap().copy_project_inputs_to = "../escape".into();
        assert!(
            contract
                .validate(&graph)
                .unwrap_err()
                .to_string()
                .contains("..")
        );

        let mut contract = full();
        contract
            .gates
            .insert("collect".into(), GateBindingKind::Artifact);
        assert!(
            contract
                .validate(&graph)
                .unwrap_err()
                .to_string()
                .contains("no gate flow")
        );

        let mut contract = full();
        contract.publication.as_mut().unwrap().flow = "packet-approval".into();
        assert!(
            contract
                .validate(&graph)
                .unwrap_err()
                .to_string()
                .contains("executable flow")
        );

        let mut contract = full();
        contract
            .publication
            .as_mut()
            .unwrap()
            .required_evidence
            .pop();
        contract
            .publication
            .as_mut()
            .unwrap()
            .required_evidence
            .push(serde_json::from_value(json!({"role": "test", "stage": "ship"})).unwrap());
        assert!(
            contract
                .validate(&graph)
                .unwrap_err()
                .to_string()
                .contains("repeats role")
        );
    }

    #[test]
    fn inconsistent_module_dependencies_fail_before_any_effect() {
        // The durable plan revision is packet-bound, so a plan declaration
        // without a packet producer is internally inconsistent.
        let graph = full_graph();
        let mut contract = full();
        contract.requirements_packet = None;
        let error = contract.validate(&graph).unwrap_err().to_string();
        assert!(error.contains("requirements_packet"), "{error}");

        // The approved packet gate must precede plan production, otherwise the
        // plan stage would run before the packet identity exists.
        let mut contract = full();
        let plan = contract.task_plan.as_mut().unwrap();
        plan.produce_flow = "collect".into();
        plan.validate_stage = "write".into();
        let error = contract.validate(&graph).unwrap_err().to_string();
        assert!(error.contains("after the approved packet gate"), "{error}");

        // Criterion-mapped publication must run after the selection gate even
        // when the declaration does not set requires_task_selection.
        let mut contract = full();
        let publication = contract.publication.as_mut().unwrap();
        publication.flow = "collect".into();
        publication.requires_task_selection = false;
        for evidence in &mut publication.required_evidence {
            evidence.stage = "write".into();
        }
        let error = contract.validate(&graph).unwrap_err().to_string();
        assert!(error.contains("after the selection gate"), "{error}");

        // Publication requires a repository workspace; a declaration that
        // cannot produce a commit must be rejected before any remote effect.
        let mut contract = full();
        contract.workspace = WorkspaceCapability {
            kind: WorkspaceMode::None,
            provisioning: None,
        };
        let error = contract.validate(&graph).unwrap_err().to_string();
        assert!(error.contains("publication requires"), "{error}");

        // Only the first verification entry per flow would be dispatched, so a
        // duplicate would be a silently ignored declaration.
        let mut contract = full();
        let duplicate = contract.verification[0].clone();
        contract.verification.push(duplicate);
        let error = contract.validate(&graph).unwrap_err().to_string();
        assert!(error.contains("more than once"), "{error}");
    }

    #[test]
    fn plan_less_publication_is_explicit_and_criterion_mapping_fails_closed() {
        let graph = graph();
        let mut contract: WorkflowCapabilities = serde_json::from_value(json!({
            "schema_version": 1,
            "workspace": {"kind": "repository", "provisioning": "optional"},
            "publication": {
                "flow": "publish",
                "required_evidence": [{"role": "review", "stage": "ship"}]
            }
        }))
        .unwrap();
        contract.validate(&graph).unwrap();
        assert!(contract.requirements_packet.is_none());
        assert!(contract.task_plan.is_none());

        contract.publication.as_mut().unwrap().required_evidence[0].criterion_mapping = true;
        let error = contract.validate(&graph).unwrap_err().to_string();
        assert!(error.contains("criterion_mapping"), "{error}");
    }

    #[test]
    fn contract_hash_is_canonical_and_changes_with_the_declaration() {
        let base = WorkflowCapabilities::generic();
        assert_eq!(
            contract_hash(&base).unwrap(),
            contract_hash(&base.clone()).unwrap()
        );
        let mut changed = base.clone();
        changed.workspace = WorkspaceCapability {
            kind: WorkspaceMode::Repository,
            provisioning: Some(RepositoryProvisioning::Optional),
        };
        assert_ne!(
            contract_hash(&base).unwrap(),
            contract_hash(&changed).unwrap()
        );
    }

    #[test]
    fn workspace_resolution_rejects_symlink_escape() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(temp.path(), workspace.join("outside")).unwrap();
            let error = resolve_within_workspace(&workspace, "outside/file")
                .unwrap_err()
                .to_string();
            assert!(error.contains("escapes"), "{error}");
        }
        let destination = resolve_within_workspace(&workspace, "inputs").unwrap();
        assert!(destination.starts_with(std::fs::canonicalize(&workspace).unwrap()));
    }
}
