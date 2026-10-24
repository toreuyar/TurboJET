//! Declared external-delivery gate.
//!
//! Delivery is never automatic. The explicit release API must verify that the
//! frozen contract declares `external_delivery` and that its declared
//! `after_flow` has actually completed before a release request is accepted.

use anyhow::{Context, Result};
use tj_config::capability::ExternalDeliveryCapability;
use tj_db::{Database, FlowStatus};

pub fn ensure_delivery_allowed(
    db: &Database,
    run_id: &str,
    declared: Option<&ExternalDeliveryCapability>,
) -> Result<()> {
    let delivery = declared.context(
        "workflow does not declare external_delivery; no external CI/CD release is available for this run",
    )?;
    let completed = db.load_flows_for_run(run_id)?.into_iter().any(|flow| {
        flow.flow_name() == delivery.after_flow && *flow.status() == FlowStatus::Completed
    });
    anyhow::ensure!(
        completed,
        "external_delivery.after_flow '{}' has not completed for this run; no external release may start",
        delivery.after_flow
    );
    anyhow::ensure!(
        delivery.explicit_request_only,
        "external_delivery must remain explicit-request-only"
    );
    let _ = delivery;
    Ok(())
}
