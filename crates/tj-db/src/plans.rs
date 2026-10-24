//! Immutable structured-plan revisions, gate bindings, reviews and Engineering
//! task selections (TJ-031/TJ-032).
//!
//! A plan revision is append-only and always names the exact approved
//! requirements-packet revision it was generated from. A planning gate binds
//! both identities in the same transaction as its waiting transition, so an
//! approval cannot mix a plan with a different packet. A reviewer revision
//! rebinds an open gate and bumps the run version, making an approval prepared
//! for the previous plan stale.

use super::*;
use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use tj_plan::{PlanContext, PlanValidationReport, TaskPlan};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanRevision {
    pub id: String,
    pub plan_id: String,
    pub run_id: String,
    pub revision: i64,
    pub schema_version: i64,
    pub content_json: String,
    pub content_hash: String,
    pub packet_revision_id: String,
    pub packet_content_hash: String,
    pub packet_id: String,
    pub packet_revision: i64,
    pub parent_revision_id: Option<String>,
    pub source_artifact_id: Option<String>,
    pub source_artifact_hash: Option<String>,
    pub flow_name: Option<String>,
    pub stage_name: Option<String>,
    pub change_request: Option<String>,
    pub validation_json: String,
    pub created_by: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanReview {
    pub id: String,
    pub plan_id: String,
    pub revision_id: String,
    pub revision: i64,
    pub content_hash: String,
    pub packet_revision_id: String,
    pub packet_content_hash: String,
    pub gate_instance_id: String,
    pub decision_id: String,
    pub actor_id: String,
    pub actor_name: String,
    pub outcome: String,
    pub reason: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatePlanBinding {
    pub gate_instance_id: String,
    pub run_id: String,
    pub plan_id: String,
    pub revision_id: String,
    pub revision: i64,
    pub content_hash: String,
    pub packet_revision_id: String,
    pub packet_content_hash: String,
    pub bound_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct NewPlanRevision {
    pub run_id: String,
    pub plan_id: Option<String>,
    pub content_json: String,
    pub parent_revision_id: Option<String>,
    pub source_packet_revision_id: Option<String>,
    pub source_artifact_id: Option<String>,
    pub source_artifact_hash: Option<String>,
    pub flow_name: Option<String>,
    pub stage_name: Option<String>,
    pub change_request: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanBundle {
    pub latest: Option<PlanRevision>,
    pub revisions: Vec<PlanRevision>,
    pub reviews: Vec<PlanReview>,
    pub approved: Option<PlanRevision>,
    pub gate_binding: Option<GatePlanBinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanTaskSelection {
    pub id: String,
    pub run_id: String,
    pub plan_id: String,
    pub plan_revision_id: String,
    pub plan_content_hash: String,
    pub task_id: String,
    pub selected_by: String,
    pub created_at: String,
    pub payload_json: String,
}

fn read_revision(row: &rusqlite::Row<'_>) -> rusqlite::Result<PlanRevision> {
    Ok(PlanRevision {
        id: row.get(0)?,
        plan_id: row.get(1)?,
        run_id: row.get(2)?,
        revision: row.get(3)?,
        schema_version: row.get(4)?,
        content_json: row.get(5)?,
        content_hash: row.get(6)?,
        packet_revision_id: row.get(7)?,
        packet_content_hash: row.get(8)?,
        packet_id: row.get(9)?,
        packet_revision: row.get(10)?,
        parent_revision_id: row.get(11)?,
        source_artifact_id: row.get(12)?,
        source_artifact_hash: row.get(13)?,
        flow_name: row.get(14)?,
        stage_name: row.get(15)?,
        change_request: row.get(16)?,
        validation_json: row.get(17)?,
        created_by: row.get(18)?,
        created_at: row.get(19)?,
    })
}

const REVISION_COLUMNS_P: &str =
    "p.id, p.plan_id, p.run_id, p.revision, p.schema_version, p.content_json, p.content_hash,
     p.packet_revision_id, p.packet_content_hash, p.packet_id, p.packet_revision,
     p.parent_revision_id, p.source_artifact_id, p.source_artifact_hash, p.flow_name,
     p.stage_name, p.change_request, p.validation_json, p.created_by, p.created_at";

const REVISION_COLUMNS: &str =
    "id, plan_id, run_id, revision, schema_version, content_json, content_hash,
     packet_revision_id, packet_content_hash, packet_id, packet_revision,
     parent_revision_id, source_artifact_id, source_artifact_hash, flow_name,
     stage_name, change_request, validation_json, created_by, created_at";

fn read_review(row: &rusqlite::Row<'_>) -> rusqlite::Result<PlanReview> {
    Ok(PlanReview {
        id: row.get(0)?,
        plan_id: row.get(1)?,
        revision_id: row.get(2)?,
        revision: row.get(3)?,
        content_hash: row.get(4)?,
        packet_revision_id: row.get(5)?,
        packet_content_hash: row.get(6)?,
        gate_instance_id: row.get(7)?,
        decision_id: row.get(8)?,
        actor_id: row.get(9)?,
        actor_name: row.get(10)?,
        outcome: row.get(11)?,
        reason: row.get(12)?,
        created_at: row.get(13)?,
    })
}

fn read_selection(row: &rusqlite::Row<'_>) -> rusqlite::Result<PlanTaskSelection> {
    Ok(PlanTaskSelection {
        id: row.get(0)?,
        run_id: row.get(1)?,
        plan_id: row.get(2)?,
        plan_revision_id: row.get(3)?,
        plan_content_hash: row.get(4)?,
        task_id: row.get(5)?,
        selected_by: row.get(6)?,
        created_at: row.get(7)?,
        payload_json: row.get(8)?,
    })
}

pub(crate) fn migrate(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS plan_revisions (
            id TEXT PRIMARY KEY,
            plan_id TEXT NOT NULL,
            run_id TEXT NOT NULL,
            revision INTEGER NOT NULL,
            schema_version INTEGER NOT NULL,
            content_json TEXT NOT NULL,
            content_hash TEXT NOT NULL,
            packet_revision_id TEXT NOT NULL,
            packet_content_hash TEXT NOT NULL,
            packet_id TEXT NOT NULL,
            packet_revision INTEGER NOT NULL,
            parent_revision_id TEXT,
            source_artifact_id TEXT,
            source_artifact_hash TEXT,
            flow_name TEXT,
            stage_name TEXT,
            change_request TEXT,
            validation_json TEXT NOT NULL,
            created_by TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_plan_revisions_number
            ON plan_revisions(plan_id, revision);
        CREATE INDEX IF NOT EXISTS idx_plan_revisions_run
            ON plan_revisions(run_id, revision);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_plan_revisions_run_hash
            ON plan_revisions(run_id, content_hash);
        CREATE INDEX IF NOT EXISTS idx_plan_revisions_packet
            ON plan_revisions(packet_revision_id);

        CREATE TABLE IF NOT EXISTS run_gate_plans (
            gate_instance_id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL,
            plan_id TEXT NOT NULL,
            revision_id TEXT NOT NULL,
            revision INTEGER NOT NULL,
            content_hash TEXT NOT NULL,
            packet_revision_id TEXT NOT NULL,
            packet_content_hash TEXT NOT NULL,
            bound_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_run_gate_plans_run ON run_gate_plans(run_id);

        CREATE TABLE IF NOT EXISTS plan_reviews (
            id TEXT PRIMARY KEY,
            plan_id TEXT NOT NULL,
            revision_id TEXT NOT NULL,
            revision INTEGER NOT NULL,
            content_hash TEXT NOT NULL,
            packet_revision_id TEXT NOT NULL,
            packet_content_hash TEXT NOT NULL,
            gate_instance_id TEXT NOT NULL,
            decision_id TEXT NOT NULL,
            actor_id TEXT NOT NULL,
            actor_name TEXT NOT NULL,
            outcome TEXT NOT NULL,
            reason TEXT,
            created_at TEXT NOT NULL
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_plan_reviews_decision ON plan_reviews(decision_id);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_plan_reviews_approval
            ON plan_reviews(revision_id) WHERE outcome = 'approved';
        CREATE INDEX IF NOT EXISTS idx_plan_reviews_revision ON plan_reviews(revision_id);

        CREATE TABLE IF NOT EXISTS plan_task_selections (
            id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL,
            plan_id TEXT NOT NULL,
            plan_revision_id TEXT NOT NULL,
            plan_content_hash TEXT NOT NULL,
            task_id TEXT NOT NULL,
            selected_by TEXT NOT NULL,
            created_at TEXT NOT NULL,
            payload_json TEXT NOT NULL
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_plan_task_selection
            ON plan_task_selections(plan_revision_id, task_id);
        CREATE INDEX IF NOT EXISTS idx_plan_task_selections_run
            ON plan_task_selections(run_id, created_at);",
    )?;
    for (column, declaration) in [("plan_revision_id", "TEXT"), ("plan_content_hash", "TEXT")] {
        if conn
            .prepare(&format!("SELECT {column} FROM gate_decisions LIMIT 0"))
            .is_err()
        {
            conn.execute_batch(&format!(
                "ALTER TABLE gate_decisions ADD COLUMN {column} {declaration};"
            ))?;
        }
    }
    Ok(())
}

/// The latest approved packet revision for this run. A plan may only be built
/// from this exact row, never from "whatever packet is latest now".
pub fn approved_packet_revision(
    conn: &rusqlite::Connection,
    run_id: &str,
) -> Result<Option<packets::PacketRevision>> {
    conn.query_row(
        "SELECT p.id, p.packet_id, p.run_id, p.revision, p.schema_version, p.content_json,
                p.content_hash, p.completeness, p.parent_revision_id, p.source_artifact_id,
                p.source_artifact_hash, p.flow_name, p.stage_name, p.change_request,
                p.created_by, p.created_at
         FROM packet_revisions p
         JOIN packet_reviews r ON r.revision_id = p.id AND r.outcome = 'approved'
         WHERE p.run_id = ?1
         ORDER BY r.created_at DESC, p.revision DESC LIMIT 1",
        params![run_id],
        |row| {
            Ok(packets::PacketRevision {
                id: row.get(0)?,
                packet_id: row.get(1)?,
                run_id: row.get(2)?,
                revision: row.get(3)?,
                schema_version: row.get(4)?,
                content_json: row.get(5)?,
                content_hash: row.get(6)?,
                completeness: row.get(7)?,
                parent_revision_id: row.get(8)?,
                source_artifact_id: row.get(9)?,
                source_artifact_hash: row.get(10)?,
                flow_name: row.get(11)?,
                stage_name: row.get(12)?,
                change_request: row.get(13)?,
                created_by: row.get(14)?,
                created_at: row.get(15)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn normalize_plan(
    raw: &str,
    approved: &packets::PacketRevision,
) -> Result<(TaskPlan, PlanValidationReport)> {
    let mut plan = TaskPlan::from_json(raw).map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let source = tj_plan::PlanSourcePacket {
        packet_id: approved.packet_id.clone(),
        revision: approved.revision,
        content_hash: approved.content_hash.clone(),
    };
    match &plan.source_packet {
        Some(claimed) if claimed != &source => {
            anyhow::bail!(
                "plan names packet '{}' r{} {} but the accepted input is '{}' r{} {}",
                claimed.packet_id,
                claimed.revision,
                claimed.content_hash,
                source.packet_id,
                source.revision,
                source.content_hash
            );
        }
        Some(_) => {}
        None => plan.source_packet = Some(source),
    }
    let packet_value: serde_json::Value = serde_json::from_str(&approved.content_json)
        .context("approved packet content is unreadable")?;
    let context = PlanContext::from_packet_value(&packet_value);
    let report = plan
        .validate(Some(&context))
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    Ok((plan, report))
}

impl Database {
    pub fn insert_plan_revision(&self, draft: &NewPlanRevision) -> Result<(PlanRevision, bool)> {
        anyhow::ensure!(
            !draft.created_by.trim().is_empty(),
            "plan revision creator is required"
        );
        let mut conn = lock_conn!(self);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let run_status: String = tx
            .query_row(
                "SELECT status FROM pipeline_runs WHERE id = ?1",
                params![draft.run_id],
                |row| row.get(0),
            )
            .optional()?
            .context("Run not found")?;
        anyhow::ensure!(
            matches!(run_status.as_str(), "running" | "waiting_for_gate"),
            "Plan revisions cannot be written after the run is {run_status}"
        );
        let approved = approved_packet_revision(&tx, &draft.run_id)?
            .context("Planning requires an approved requirements packet revision; approve the Discovery packet first")?;
        if let Some(claimed) = draft.source_packet_revision_id.as_deref() {
            anyhow::ensure!(
                claimed == approved.id,
                "Plan source packet revision is not the approved input for this run"
            );
        }
        let (plan, report) = normalize_plan(&draft.content_json, &approved)?;
        let content_hash = plan.content_hash();
        let normalized = serde_json::to_string(&plan)?;

        let latest: Option<PlanRevision> = tx.query_row(
            &format!("SELECT {REVISION_COLUMNS} FROM plan_revisions WHERE run_id = ?1 ORDER BY revision DESC LIMIT 1"),
            params![draft.run_id], read_revision,
        ).optional()?;
        if let Some(latest) = &latest {
            if draft
                .plan_id
                .as_deref()
                .is_some_and(|id| id != latest.plan_id)
            {
                anyhow::bail!("Plan id does not belong to this run's plan lineage");
            }
            let approved_latest: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM plan_reviews WHERE revision_id = ?1 AND outcome = 'approved')",
                params![latest.id], |row| row.get(0),
            )?;
            anyhow::ensure!(
                !approved_latest,
                "The approved plan revision is immutable; scope changes require a new packet revision and plan"
            );
            if latest.content_hash == content_hash {
                anyhow::ensure!(
                    draft.change_request.is_none(),
                    "Revision content is unchanged; edit the plan before requesting a new revision"
                );
                return Ok((latest.clone(), true));
            }
        }
        let plan_id = draft
            .plan_id
            .clone()
            .or_else(|| latest.as_ref().map(|item| item.plan_id.clone()))
            .unwrap_or_else(|| format!("pln_{}", uuid::Uuid::new_v4()));
        let revision = latest.as_ref().map_or(1, |item| item.revision + 1);
        let parent_revision_id = draft
            .parent_revision_id
            .clone()
            .or_else(|| latest.as_ref().map(|item| item.id.clone()));
        let id = format!("plr_{}", uuid::Uuid::new_v4());
        let created_at = chrono::Utc::now().to_rfc3339();
        let validation_json = serde_json::to_string(&report)?;
        tx.execute(
            "INSERT INTO plan_revisions
             (id, plan_id, run_id, revision, schema_version, content_json, content_hash,
              packet_revision_id, packet_content_hash, packet_id, packet_revision,
              parent_revision_id, source_artifact_id, source_artifact_hash, flow_name,
              stage_name, change_request, validation_json, created_by, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
            params![
                id, plan_id, draft.run_id, revision, plan.schema_version as i64, normalized,
                content_hash, approved.id, approved.content_hash, approved.packet_id,
                approved.revision, parent_revision_id, draft.source_artifact_id,
                draft.source_artifact_hash, draft.flow_name, draft.stage_name,
                draft.change_request, validation_json, draft.created_by, created_at
            ],
        )?;
        tx.execute(
            "INSERT INTO events (id, run_id, kind, flow_name, stage_name, summary, payload, created_at)
             VALUES (?1, ?2, 'plan_revision', ?3, ?4, ?5, ?6, ?7)",
            params![
                format!("evt_{}", uuid::Uuid::new_v4()), draft.run_id, draft.flow_name, draft.stage_name,
                draft.change_request.as_deref().unwrap_or("generated structured task plan"),
                serde_json::json!({
                    "plan_id": plan_id, "revision_id": id, "revision": revision,
                    "content_hash": content_hash, "packet_revision_id": approved.id,
                    "packet_content_hash": approved.content_hash,
                    "task_count": report.task_count,
                    "requires_packet_revision": report.requires_packet_revision,
                    "blocked_tasks": report.blocked_tasks, "created_by": draft.created_by
                }).to_string(), created_at
            ],
        )?;
        // A new plan revision while a planning gate is open rebinds that review
        // and bumps the run version. An approval prepared for the previous
        // plan/hash now conflicts.
        let rebind = tx.execute(
            "UPDATE run_gate_plans
             SET plan_id = ?1, revision_id = ?2, revision = ?3, content_hash = ?4,
                 packet_revision_id = ?5, packet_content_hash = ?6, updated_at = ?7
             WHERE run_id = ?8 AND gate_instance_id = (
                 SELECT gate_instance_id FROM pipeline_runs WHERE id = ?8 AND status = 'waiting_for_gate'
             )",
            params![plan_id, id, revision, content_hash, approved.id, approved.content_hash, created_at, draft.run_id],
        )?;
        if rebind > 0 {
            tx.execute(
                "UPDATE pipeline_runs SET revision = revision + 1, updated_at = ?2
                 WHERE id = ?1 AND status = 'waiting_for_gate'",
                params![draft.run_id, created_at],
            )?;
        }
        let inserted = tx.query_row(
            &format!("SELECT {REVISION_COLUMNS} FROM plan_revisions WHERE id = ?1"),
            params![id],
            read_revision,
        )?;
        tx.commit()?;
        Ok((inserted, false))
    }

    pub fn plan_revision(&self, revision_id: &str) -> Result<Option<PlanRevision>> {
        let conn = lock_conn!(self);
        conn.query_row(
            &format!("SELECT {REVISION_COLUMNS} FROM plan_revisions WHERE id = ?1"),
            params![revision_id],
            read_revision,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn approved_packet_revision(
        &self,
        run_id: &str,
    ) -> Result<Option<packets::PacketRevision>> {
        let conn = lock_conn!(self);
        approved_packet_revision(&conn, run_id)
    }

    pub fn approved_plan_revision(&self, run_id: &str) -> Result<Option<PlanRevision>> {
        let conn = lock_conn!(self);
        conn.query_row(
            &format!(
                "SELECT {REVISION_COLUMNS_P} FROM plan_revisions p
                 JOIN plan_reviews r ON r.revision_id = p.id AND r.outcome = 'approved'
                 WHERE p.run_id = ?1 ORDER BY r.created_at DESC, p.revision DESC LIMIT 1"
            ),
            params![run_id],
            read_revision,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn plan_bundle(&self, run_id: &str) -> Result<PlanBundle> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(&format!(
            "SELECT {REVISION_COLUMNS} FROM plan_revisions WHERE run_id = ?1 ORDER BY revision"
        ))?;
        let revisions = stmt
            .query_map(params![run_id], read_revision)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut reviews_stmt = conn.prepare(
            "SELECT id, plan_id, revision_id, revision, content_hash, packet_revision_id,
                    packet_content_hash, gate_instance_id, decision_id, actor_id, actor_name,
                    outcome, reason, created_at
             FROM plan_reviews WHERE plan_id IN (SELECT DISTINCT plan_id FROM plan_revisions WHERE run_id = ?1)
             ORDER BY created_at, rowid",
        )?;
        let reviews = reviews_stmt
            .query_map(params![run_id], read_review)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let latest = revisions.last().cloned();
        let approved = reviews
            .iter()
            .rev()
            .find(|review| review.outcome == "approved")
            .and_then(|review| {
                revisions
                    .iter()
                    .find(|revision| revision.id == review.revision_id)
                    .cloned()
            });
        let gate_binding = conn
            .query_row(
                "SELECT b.gate_instance_id, b.run_id, b.plan_id, b.revision_id, b.revision,
                    b.content_hash, b.packet_revision_id, b.packet_content_hash,
                    b.bound_at, b.updated_at
             FROM run_gate_plans b JOIN pipeline_runs r ON r.id = b.run_id
             WHERE b.run_id = ?1 AND r.gate_instance_id = b.gate_instance_id",
                params![run_id],
                |row| {
                    Ok(GatePlanBinding {
                        gate_instance_id: row.get(0)?,
                        run_id: row.get(1)?,
                        plan_id: row.get(2)?,
                        revision_id: row.get(3)?,
                        revision: row.get(4)?,
                        content_hash: row.get(5)?,
                        packet_revision_id: row.get(6)?,
                        packet_content_hash: row.get(7)?,
                        bound_at: row.get(8)?,
                        updated_at: row.get(9)?,
                    })
                },
            )
            .optional()?;
        Ok(PlanBundle {
            latest,
            revisions,
            reviews,
            approved,
            gate_binding,
        })
    }

    pub fn gate_plan_binding(&self, gate_instance_id: &str) -> Result<Option<GatePlanBinding>> {
        let conn = lock_conn!(self);
        conn.query_row(
            "SELECT gate_instance_id, run_id, plan_id, revision_id, revision, content_hash,
                    packet_revision_id, packet_content_hash, bound_at, updated_at
             FROM run_gate_plans WHERE gate_instance_id = ?1",
            params![gate_instance_id],
            |row| {
                Ok(GatePlanBinding {
                    gate_instance_id: row.get(0)?,
                    run_id: row.get(1)?,
                    plan_id: row.get(2)?,
                    revision_id: row.get(3)?,
                    revision: row.get(4)?,
                    content_hash: row.get(5)?,
                    packet_revision_id: row.get(6)?,
                    packet_content_hash: row.get(7)?,
                    bound_at: row.get(8)?,
                    updated_at: row.get(9)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn insert_plan_task_selection(
        &self,
        run_id: &str,
        revision: &PlanRevision,
        task_id: &str,
        selected_by: &str,
        payload_json: String,
    ) -> Result<(PlanTaskSelection, bool)> {
        anyhow::ensure!(
            revision.run_id == run_id,
            "Plan revision belongs to another run"
        );
        anyhow::ensure!(
            !selected_by.trim().is_empty(),
            "Selected-by identity is required"
        );
        let mut conn = lock_conn!(self);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        // The selection must name the *current* approved plan revision. An
        // older revision that was approved before a reviewer edit is history,
        // not a valid handoff to the next flow, even though its review row remains.
        let latest_revision_id: Option<String> = tx
            .query_row(
                "SELECT id FROM plan_revisions WHERE run_id = ?1 ORDER BY revision DESC LIMIT 1",
                params![run_id],
                |row| row.get(0),
            )
            .optional()?;
        let current_approved_id: Option<String> = tx
            .query_row(
                "SELECT r.revision_id FROM plan_reviews r
             JOIN plan_revisions p ON p.id = r.revision_id
             WHERE p.run_id = ?1 AND r.outcome = 'approved'
             ORDER BY r.created_at DESC, r.rowid DESC LIMIT 1",
                params![run_id],
                |row| row.get(0),
            )
            .optional()?;
        anyhow::ensure!(
            latest_revision_id.as_deref() == Some(revision.id.as_str()),
            "A task may only be selected from the latest plan revision; an earlier approved revision is stale"
        );
        anyhow::ensure!(
            current_approved_id.as_deref() == Some(revision.id.as_str()),
            "A task may only be selected from the current approved plan revision"
        );
        if let Some(existing) = tx
            .query_row(
                "SELECT id, run_id, plan_id, plan_revision_id, plan_content_hash, task_id,
                    selected_by, created_at, payload_json
             FROM plan_task_selections WHERE plan_revision_id = ?1 AND task_id = ?2",
                params![revision.id, task_id],
                read_selection,
            )
            .optional()?
        {
            return Ok((existing, true));
        }
        let other_task: Option<String> = tx
            .query_row(
                "SELECT task_id FROM plan_task_selections WHERE plan_revision_id = ?1 LIMIT 1",
                params![revision.id],
                |row| row.get(0),
            )
            .optional()?;
        anyhow::ensure!(
            other_task.is_none(),
            "Plan revision already has a selected task ('{}'); one task is allowed per attempt/workspace — start a new attempt to select another task",
            other_task.unwrap_or_default()
        );
        let id = format!("pts_{}", uuid::Uuid::new_v4());
        let created_at = chrono::Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO plan_task_selections
             (id, run_id, plan_id, plan_revision_id, plan_content_hash, task_id,
              selected_by, created_at, payload_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                id,
                run_id,
                revision.plan_id,
                revision.id,
                revision.content_hash,
                task_id,
                selected_by,
                created_at,
                payload_json
            ],
        )?;
        tx.execute(
            "INSERT INTO events (id, run_id, kind, flow_name, stage_name, summary, payload, created_at)
             VALUES (?1, ?2, 'plan_task_selected', ?3, NULL, ?4, ?5, ?6)",
            params![
                format!("evt_{}", uuid::Uuid::new_v4()), run_id,
                revision.flow_name.as_deref(),
                format!("task {task_id} selected for the next flow"),
                payload_json, created_at
            ],
        )?;
        let inserted = tx.query_row(
            "SELECT id, run_id, plan_id, plan_revision_id, plan_content_hash, task_id,
                    selected_by, created_at, payload_json
             FROM plan_task_selections WHERE id = ?1",
            params![id],
            read_selection,
        )?;
        tx.commit()?;
        Ok((inserted, false))
    }

    pub fn plan_task_selections(&self, run_id: &str) -> Result<Vec<PlanTaskSelection>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(
            "SELECT id, run_id, plan_id, plan_revision_id, plan_content_hash, task_id,
                    selected_by, created_at, payload_json
             FROM plan_task_selections WHERE run_id = ?1 ORDER BY created_at, rowid",
        )?;
        stmt.query_map(params![run_id], read_selection)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }
}

pub(crate) fn bind_gate_plan_tx(
    tx: &Transaction<'_>,
    run_id: &str,
    gate_instance_id: &str,
    revision: &PlanRevision,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    tx.execute(
        "INSERT INTO run_gate_plans
         (gate_instance_id, run_id, plan_id, revision_id, revision, content_hash,
          packet_revision_id, packet_content_hash, bound_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)
         ON CONFLICT(gate_instance_id) DO UPDATE SET
            plan_id=excluded.plan_id, revision_id=excluded.revision_id, revision=excluded.revision,
            content_hash=excluded.content_hash, packet_revision_id=excluded.packet_revision_id,
            packet_content_hash=excluded.packet_content_hash, updated_at=excluded.updated_at",
        params![
            gate_instance_id,
            run_id,
            revision.plan_id,
            revision.id,
            revision.revision,
            revision.content_hash,
            revision.packet_revision_id,
            revision.packet_content_hash,
            now
        ],
    )?;
    Ok(())
}

pub(crate) fn insert_plan_review_tx(
    tx: &Transaction<'_>,
    decision: &GateDecision,
    decision_id: &str,
    binding: &GatePlanBinding,
    review_id: &str,
    created_at: &str,
) -> Result<()> {
    tx.execute(
        "INSERT INTO plan_reviews
         (id, plan_id, revision_id, revision, content_hash, packet_revision_id,
          packet_content_hash, gate_instance_id, decision_id, actor_id, actor_name,
          outcome, reason, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            review_id,
            binding.plan_id,
            binding.revision_id,
            binding.revision,
            binding.content_hash,
            binding.packet_revision_id,
            binding.packet_content_hash,
            binding.gate_instance_id,
            decision_id,
            decision.actor_id,
            decision.actor_name,
            if decision.approved {
                "approved"
            } else {
                "changes_requested"
            },
            decision.reason,
            created_at
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAN: &str = include_str!("../../tj-plan/fixtures/complete.json");

    fn db_with_approved_packet() -> (Database, std::path::PathBuf, packets::PacketRevision) {
        let dir = std::env::temp_dir().join(format!("tj-plans-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Database::open(&dir.join("state.db").to_string_lossy()).unwrap();
        db.save_project(&Project {
            id: "project".into(),
            name: "project".into(),
            repo_url: None,
            base_branch: "main".into(),
            workspace_path: None,
            git_provider: None,
            profile: "full".into(),
            workspace_kind: None,
            catalog_id: None,
            provisioning_state: None,
            provisioning_error: None,
            created_at: String::new(),
            updated_at: String::new(),
        })
        .unwrap();
        db.create_user(&User {
            id: "alice".into(),
            username: "alice".into(),
            password_hash: "unused".into(),
            display_name: Some("Alice".into()),
            is_admin: true,
            created_at: String::new(),
        })
        .unwrap();
        db.set_project_member("alice", "project", "alice", true)
            .unwrap();
        db.save_run(&PipelineRun::new(
            "run".into(),
            Some(ProjectId("project".into())),
            None,
            "plan".into(),
            RunStatus::Running,
            None,
            None,
            Some("trace".into()),
            None,
        ))
        .unwrap();
        let packet_draft = packets::NewPacketRevision {
            run_id: "run".into(),
            packet_id: None,
            content_json: include_str!("../../tj-packet/fixtures/complete.json").into(),
            parent_revision_id: None,
            source_artifact_id: Some("art_packet".into()),
            source_artifact_hash: Some("hash".into()),
            flow_name: Some("flow-a".into()),
            stage_name: Some("artifact-a".into()),
            change_request: None,
            created_by: "system:packet".into(),
            allowed_sources: None,
        };
        let (packet, _) = db.insert_packet_revision(&packet_draft).unwrap();
        let version = db
            .enter_gate_bound(
                "run",
                0,
                "gate_discovery",
                "gate-a",
                "review",
                Some(&packet),
            )
            .unwrap();
        let decision = GateDecision {
            id: String::new(),
            run_id: RunId("run".into()),
            gate_name: String::new(),
            actor_id: "alice".into(),
            actor_name: "Alice".into(),
            approved: true,
            reason: None,
            note: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            gate_instance_id: Some("gate_discovery".into()),
            idempotency_key: Some("approve-packet".into()),
            expected_state_version: Some(version),
            state_version: None,
            disposition: None,
            packet_revision_id: Some(packet.id.clone()),
            packet_content_hash: Some(packet.content_hash.clone()),
            plan_revision_id: None,
            plan_content_hash: None,
            output_artifact_id: None,
            output_content_hash: None,
            output_stage_name: None,
            acknowledge_incomplete: false,
        };
        db.decide_gate(&decision, version, 3).unwrap();
        (db, dir, packet)
    }

    fn draft(content: &str, packet: &packets::PacketRevision) -> NewPlanRevision {
        NewPlanRevision {
            run_id: "run".into(),
            plan_id: None,
            content_json: content.into(),
            parent_revision_id: None,
            source_packet_revision_id: Some(packet.id.clone()),
            source_artifact_id: Some("art_plan".into()),
            source_artifact_hash: Some("hash".into()),
            flow_name: Some("planning".into()),
            stage_name: Some("plan".into()),
            change_request: None,
            created_by: "system:plan-generation".into(),
        }
    }

    fn plan_decision(
        key: &str,
        approved: bool,
        plan: &PlanRevision,
        packet: &packets::PacketRevision,
    ) -> GateDecision {
        GateDecision {
            id: String::new(),
            run_id: RunId("run".into()),
            gate_name: String::new(),
            actor_id: "alice".into(),
            actor_name: "Alice".into(),
            approved,
            reason: if approved {
                None
            } else {
                Some("plan needs work".into())
            },
            note: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            gate_instance_id: Some("gate_planning".into()),
            idempotency_key: Some(key.into()),
            expected_state_version: Some(1),
            state_version: None,
            disposition: None,
            packet_revision_id: Some(packet.id.clone()),
            packet_content_hash: Some(packet.content_hash.clone()),
            plan_revision_id: Some(plan.id.clone()),
            plan_content_hash: Some(plan.content_hash.clone()),
            output_artifact_id: None,
            output_content_hash: None,
            output_stage_name: None,
            acknowledge_incomplete: false,
        }
    }

    #[test]
    fn plan_revision_is_bound_to_the_exact_approved_packet_and_deduplicated() {
        let (db, _dir, packet) = db_with_approved_packet();
        let (revision, replay) = db.insert_plan_revision(&draft(PLAN, &packet)).unwrap();
        assert!(!replay);
        assert_eq!(revision.revision, 1);
        assert_eq!(revision.packet_revision_id, packet.id);
        assert_eq!(revision.packet_content_hash, packet.content_hash);
        let normalized: tj_plan::TaskPlan = serde_json::from_str(&revision.content_json).unwrap();
        let source = normalized.source_packet.unwrap();
        assert_eq!(source.packet_id, packet.packet_id);
        assert_eq!(source.revision, packet.revision);
        assert_eq!(source.content_hash, packet.content_hash);
        let report: serde_json::Value = serde_json::from_str(&revision.validation_json).unwrap();
        assert_eq!(report["valid"], true);
        assert_eq!(report["task_count"], 4);

        let (again, replay) = db.insert_plan_revision(&draft(PLAN, &packet)).unwrap();
        assert!(replay);
        assert_eq!(again.id, revision.id);
        assert_eq!(db.plan_bundle("run").unwrap().revisions.len(), 1);
    }

    #[test]
    fn plan_claiming_another_packet_revision_is_rejected() {
        let (db, _dir, packet) = db_with_approved_packet();
        let mut value: serde_json::Value = serde_json::from_str(PLAN).unwrap();
        value["source_packet"] = serde_json::json!({
            "packet_id": packet.packet_id,
            "revision": packet.revision + 1,
            "content_hash": "f".repeat(64)
        });
        let error = db
            .insert_plan_revision(&draft(&value.to_string(), &packet))
            .unwrap_err()
            .to_string();
        assert!(error.contains("accepted input"), "{error}");
        assert!(db.plan_bundle("run").unwrap().revisions.is_empty());
    }

    #[test]
    fn new_plan_revision_rebinds_gate_and_stale_plan_approval_conflicts() {
        let (db, _dir, packet) = db_with_approved_packet();
        let (first, _) = db.insert_plan_revision(&draft(PLAN, &packet)).unwrap();
        let version = db
            .enter_gate_bound_to(
                "run",
                db.load_run("run").unwrap().unwrap().state_version(),
                "gate_planning",
                "planning-gate",
                "review the plan",
                Some(&packet),
                Some(&first),
            )
            .unwrap();
        let binding = db.gate_plan_binding("gate_planning").unwrap().unwrap();
        assert_eq!(binding.revision_id, first.id);
        assert_eq!(binding.packet_revision_id, packet.id);

        let mut changed: serde_json::Value = serde_json::from_str(PLAN).unwrap();
        changed["title"] = serde_json::json!("Revised plan title");
        let (second, _) = db
            .insert_plan_revision(&draft(&changed.to_string(), &packet))
            .unwrap();
        assert_eq!(second.revision, 2);
        assert_eq!(
            second.parent_revision_id.as_deref(),
            Some(first.id.as_str())
        );
        let after = db.load_run("run").unwrap().unwrap().state_version();
        assert!(after > version);
        assert_eq!(
            db.gate_plan_binding("gate_planning")
                .unwrap()
                .unwrap()
                .revision_id,
            second.id
        );

        let stale = plan_decision("stale-plan", true, &first, &packet);
        let error = db.decide_gate(&stale, after, 3).unwrap_err().to_string();
        assert!(error.contains("plan revision/hash is stale"), "{error}");
        assert!(db.plan_bundle("run").unwrap().reviews.is_empty());

        let exact = plan_decision("exact-plan", true, &second, &packet);
        let receipt = db.decide_gate(&exact, after, 3).unwrap();
        assert_eq!(
            receipt.plan_revision_id.as_deref(),
            Some(second.id.as_str())
        );
        assert_eq!(
            receipt.packet_revision_id.as_deref(),
            Some(packet.id.as_str())
        );
        let bundle = db.plan_bundle("run").unwrap();
        assert_eq!(bundle.reviews.len(), 1);
        assert_eq!(bundle.reviews[0].outcome, "approved");
        assert_eq!(bundle.approved.unwrap().id, second.id);
    }

    #[test]
    fn engineering_task_selection_requires_approved_plan_and_replays_idempotently() {
        let (db, _dir, packet) = db_with_approved_packet();
        let (plan, _) = db.insert_plan_revision(&draft(PLAN, &packet)).unwrap();
        let error = db
            .insert_plan_task_selection("run", &plan, "T-1", "alice", "{}".into())
            .unwrap_err()
            .to_string();
        assert!(error.contains("approved plan"), "{error}");

        let version = db
            .enter_gate_bound_to(
                "run",
                db.load_run("run").unwrap().unwrap().state_version(),
                "gate_planning",
                "planning-gate",
                "review the plan",
                Some(&packet),
                Some(&plan),
            )
            .unwrap();
        db.decide_gate(
            &plan_decision("approve-plan", true, &plan, &packet),
            version,
            3,
        )
        .unwrap();

        let payload =
            serde_json::json!({"task_id":"T-1","acceptance_criteria":["criterion"]}).to_string();
        let (selection, replay) = db
            .insert_plan_task_selection("run", &plan, "T-1", "alice", payload.clone())
            .unwrap();
        assert!(!replay);
        assert_eq!(selection.plan_content_hash, plan.content_hash);
        assert_eq!(selection.task_id, "T-1");
        let (again, replay) = db
            .insert_plan_task_selection("run", &plan, "T-1", "alice", payload)
            .unwrap();
        assert!(replay);
        assert_eq!(again.id, selection.id);
        assert_eq!(db.plan_task_selections("run").unwrap().len(), 1);
        let conflict = db
            .insert_plan_task_selection("run", &plan, "T-2", "alice", "{}".into())
            .unwrap_err()
            .to_string();
        assert!(conflict.contains("one task"), "{conflict}");
        assert!(
            db.list_events("run")
                .unwrap()
                .iter()
                .any(|event| event.kind == "plan_task_selected")
        );
    }
}
