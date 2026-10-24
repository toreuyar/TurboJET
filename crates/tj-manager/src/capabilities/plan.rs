//! Declared structured task-plan production and selection module.
//!
//! The produce flow, validate stage and binding gate come from the frozen
//! contract. Selection identity is recomputed from durable approved packet/plan
//! rows; no flow, stage or gate name is interpreted.

use anyhow::{Context, Result};
use std::collections::HashSet;
use tj_config::capability::TaskPlanCapability;
use tj_db::Database;
use tj_db::plans::{PlanRevision, PlanTaskSelection};

/// Persist the generated plan from the declared produce flow. The plan is
/// bound to the exact approved packet revision by the database transaction.
pub fn record_generated_plan(
    db: &Database,
    run_id: &str,
    capability: &TaskPlanCapability,
) -> Result<Option<PlanRevision>> {
    let approved = db
        .approved_packet_revision(run_id)?
        .context("task_plan requires the exact approved requirements packet revision")?;
    let artifacts = db.list_artifacts(run_id)?;
    let candidate = artifacts
        .into_iter()
        .filter(|artifact| artifact.flow_name == capability.produce_flow)
        .filter_map(|artifact| {
            let value: serde_json::Value = serde_json::from_str(&artifact.content).ok()?;
            let plan = value.get("plan")?.clone();
            Some((artifact, plan))
        })
        .max_by(|(left, _), (right, _)| {
            left.created_at
                .cmp(&right.created_at)
                .then(left.version.cmp(&right.version))
        });
    let Some((artifact, plan_value)) = candidate else {
        return Ok(None);
    };
    let plan_json = serde_json::to_string(&plan_value)?;
    let draft = tj_db::plans::NewPlanRevision {
        run_id: run_id.to_string(),
        plan_id: None,
        content_json: plan_json,
        parent_revision_id: None,
        source_packet_revision_id: Some(approved.id.clone()),
        source_artifact_id: Some(artifact.id.clone()),
        source_artifact_hash: Some(artifact.content_hash.clone()),
        flow_name: Some(artifact.flow_name.clone()),
        stage_name: Some(artifact.stage_name.clone()),
        change_request: None,
        created_by: "system:plan-generation".to_string(),
    };
    let (revision, replayed) = db.insert_plan_revision(&draft)?;
    tracing::info!(
        run_id, plan_id = %revision.plan_id, revision = revision.revision,
        hash = %revision.content_hash, packet_revision = %revision.packet_revision_id,
        replayed, flow = %capability.produce_flow, validate_stage = %capability.validate_stage,
        "Structured task plan revision recorded from declared stage output"
    );
    Ok(Some(revision))
}

/// Exactly one selection, bound to the latest approved plan revision/hash, with
/// a ready task and a matching handoff payload. This is the generic selection
/// contract used by every declared task plan, independent of names.
pub fn valid_task_selection(db: &Database, run_id: &str) -> Result<Option<PlanTaskSelection>> {
    let Some(plan) = db.approved_plan_revision(run_id)? else {
        return Ok(None);
    };
    let selections: Vec<_> = db
        .plan_task_selections(run_id)?
        .into_iter()
        .filter(|selection| selection.plan_revision_id == plan.id)
        .collect();
    if selections.len() != 1 {
        return Ok(None);
    }
    let selection = selections.into_iter().next().unwrap();
    anyhow::ensure!(
        selection.plan_content_hash == plan.content_hash,
        "selection plan hash does not match the approved revision"
    );
    let plan_content = tj_plan::TaskPlan::from_json(&plan.content_json)
        .context("approved plan content is invalid")?;
    let task = plan_content
        .task(&selection.task_id)
        .context("selected task no longer exists in the approved plan")?;
    anyhow::ensure!(
        !task.acceptance_criteria.is_empty(),
        "selected task has no acceptance criteria"
    );
    let packet = db
        .approved_packet_revision(run_id)?
        .context("approved plan has no approved source packet revision")?;
    let packet_value: serde_json::Value = serde_json::from_str(&packet.content_json)
        .context("approved source packet content is invalid")?;
    let context = tj_plan::PlanContext::from_packet_value(&packet_value);
    let readiness = plan_content.readiness(&HashSet::new(), Some(&context));
    let task_readiness = readiness
        .iter()
        .find(|candidate| candidate.task_id == selection.task_id)
        .context("selected task has no readiness record")?;
    anyhow::ensure!(
        task_readiness.ready,
        "selected task is not ready under durable evidence: {}",
        task_readiness.blocked_reasons.join("; ")
    );
    let payload: serde_json::Value =
        serde_json::from_str(&selection.payload_json).context("selection payload is not JSON")?;
    anyhow::ensure!(
        payload
            .pointer("/task/id")
            .and_then(serde_json::Value::as_str)
            == Some(selection.task_id.as_str()),
        "selection payload task identity mismatch"
    );
    anyhow::ensure!(
        payload
            .pointer("/plan/content_hash")
            .and_then(serde_json::Value::as_str)
            == Some(plan.content_hash.as_str()),
        "selection payload plan hash mismatch"
    );
    anyhow::ensure!(
        payload
            .pointer("/readiness/ready")
            .and_then(serde_json::Value::as_bool)
            == Some(true),
        "selected task was not ready at handoff"
    );
    Ok(Some(selection))
}
