//! Capability dispatch boundary.
//!
//! Dispatch is driven by the frozen [`crate::contract::RunContract`], never by
//! flow/stage/gate/profile/file names. The modules below execute the declared
//! inputs, packet, plan/selection, publication, verification and delivery work;
//! this module only decides which fixed hook applies.

pub mod delivery;
pub mod inputs;
pub mod packet;
pub mod plan;
pub mod publication;
pub mod verification;

use crate::contract::{Precondition, RunContract};
use tj_config::capability::{GateBindingKind, PublicationCapability, VerificationCapability};

/// Data required before an executable flow starts.
#[derive(Debug, Default)]
pub struct FlowPreparation<'a> {
    pub input_destination: Option<&'a str>,
    pub approved_packet: bool,
    pub approved_plan: bool,
    pub task_selection: bool,
}

/// Data required around one gate. Producer flags are true only when the
/// completed flow is the declared producer and the gate is its declared binding
/// gate.
#[derive(Debug)]
pub struct GatePreparation<'a> {
    pub binding: GateBindingKind,
    pub produce_packet: bool,
    pub produce_plan: bool,
    pub selection_target: Option<&'a str>,
}

/// Data required after one executable flow completes.
#[derive(Debug, Default)]
pub struct FlowCompletion<'a> {
    pub verification: Option<&'a VerificationCapability>,
    pub publication: Option<&'a PublicationCapability>,
    pub delivery_after: bool,
}

pub struct CapabilityRuntime<'a> {
    contract: &'a RunContract,
}

impl<'a> CapabilityRuntime<'a> {
    pub fn new(contract: &'a RunContract) -> Self {
        Self { contract }
    }

    /// All declared optional modules are implemented in this build. This hook
    /// remains the single admission check so a future unknown module fails
    /// closed instead of being silently skipped.
    pub fn ensure_admissible(_contract: &RunContract) -> anyhow::Result<()> {
        Ok(())
    }

    pub fn before_flow(&self, flow: &str) -> FlowPreparation<'a> {
        let mut preparation = FlowPreparation {
            input_destination: self.contract.input_destination(),
            ..Default::default()
        };
        for precondition in self.contract.preconditions_for(flow) {
            match precondition {
                Precondition::ApprovedPacket => preparation.approved_packet = true,
                Precondition::ApprovedPlan => preparation.approved_plan = true,
                Precondition::TaskSelection => preparation.task_selection = true,
            }
        }
        preparation
    }

    pub fn before_gate(&self, gate: &str, completed_flow: &str) -> GatePreparation<'a> {
        let packet = self.contract.packet_producer();
        let task_plan = self.contract.plan_producer();
        GatePreparation {
            binding: self.contract.gate_binding(gate),
            produce_packet: packet.is_some_and(|packet| {
                packet.produce_flow == completed_flow && packet.binding_gate == gate
            }),
            produce_plan: task_plan.is_some_and(|plan| {
                plan.produce_flow == completed_flow && plan.binding_gate == gate
            }),
            selection_target: self
                .contract
                .selection_after_gate(gate)
                .and_then(|_| self.contract.selection_target_flow()),
        }
    }

    pub fn after_flow(&self, flow: &str) -> FlowCompletion<'a> {
        FlowCompletion {
            verification: self.contract.verification_for(flow),
            publication: self.contract.publication_for(flow),
            delivery_after: self
                .contract
                .external_delivery()
                .is_some_and(|delivery| delivery.after_flow == flow),
        }
    }

    /// A declared selection gate. Returns the declared target flow.
    pub fn after_gate(&self, gate: &str) -> Option<&'a str> {
        self.contract
            .selection_after_gate(gate)
            .and_then(|_| self.contract.selection_target_flow())
    }

    pub fn contract(&self) -> &RunContract {
        self.contract
    }
}
