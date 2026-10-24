//! Generic terminal-gate output binding.
//!
//! The full SDLC binds its gates to immutable requirements-packet and plan
//! revisions. A general-purpose flow has no business packet, but approval still
//! must name the exact output the reviewer saw. This table binds one gate
//! instance to one immutable stage artifact ID/version/hash. Packet and plan
//! bindings remain separate and unchanged.

use super::*;
use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GateOutputBinding {
    pub gate_instance_id: String,
    pub run_id: String,
    pub flow_name: String,
    pub stage_name: String,
    pub artifact_id: String,
    pub artifact_version: i64,
    pub content_hash: String,
    pub bound_at: String,
    pub updated_at: String,
}

pub(crate) fn bind_gate_output_tx(
    tx: &Transaction<'_>,
    run_id: &str,
    gate_instance_id: &str,
    artifact: &RunArtifact,
) -> Result<()> {
    anyhow::ensure!(
        artifact.run_id.as_str() == run_id,
        "Output artifact belongs to another run"
    );
    let now = chrono::Utc::now().to_rfc3339();
    tx.execute(
        "INSERT INTO run_gate_outputs
             (gate_instance_id, run_id, flow_name, stage_name, artifact_id, artifact_version,
              content_hash, bound_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(gate_instance_id) DO UPDATE SET
             run_id=excluded.run_id, flow_name=excluded.flow_name,
             stage_name=excluded.stage_name, artifact_id=excluded.artifact_id,
             artifact_version=excluded.artifact_version, content_hash=excluded.content_hash,
             updated_at=excluded.updated_at",
        params![
            gate_instance_id,
            run_id,
            artifact.flow_name,
            artifact.stage_name,
            artifact.id,
            artifact.version,
            artifact.content_hash,
            now,
            now
        ],
    )
    .context("Failed to bind generic gate output")?;
    Ok(())
}

impl Database {
    pub fn gate_output_binding(&self, gate_instance_id: &str) -> Result<Option<GateOutputBinding>> {
        let conn = lock_conn!(self);
        conn.query_row(
            "SELECT gate_instance_id, run_id, flow_name, stage_name, artifact_id,
                    artifact_version, content_hash, bound_at, updated_at
             FROM run_gate_outputs WHERE gate_instance_id = ?1",
            params![gate_instance_id],
            |row| {
                Ok(GateOutputBinding {
                    gate_instance_id: row.get(0)?,
                    run_id: row.get(1)?,
                    flow_name: row.get(2)?,
                    stage_name: row.get(3)?,
                    artifact_id: row.get(4)?,
                    artifact_version: row.get(5)?,
                    content_hash: row.get(6)?,
                    bound_at: row.get(7)?,
                    updated_at: row.get(8)?,
                })
            },
        )
        .optional()
        .context("Failed to load generic gate output binding")
    }

    pub fn latest_artifact_for_flow(
        &self,
        run_id: &str,
        flow_name: &str,
    ) -> Result<Option<RunArtifact>> {
        let conn = lock_conn!(self);
        conn.query_row(
            "SELECT id, run_id, flow_name, stage_name, attempt, version, content, content_hash, created_at
             FROM artifacts WHERE run_id = ?1 AND flow_name = ?2
             ORDER BY created_at DESC, version DESC, rowid DESC LIMIT 1",
            params![run_id, flow_name],
            |row| {
                Ok(RunArtifact {
                    id: row.get(0)?,
                    run_id: RunId(row.get(1)?),
                    flow_name: row.get(2)?,
                    stage_name: row.get(3)?,
                    attempt: row.get(4)?,
                    version: row.get(5)?,
                    content: row.get(6)?,
                    content_hash: row.get(7)?,
                    created_at: row.get(8)?,
                })
            },
        )
        .optional()
        .context("Failed to load latest flow artifact")
    }

    pub fn artifact_by_id(&self, id: &str) -> Result<Option<RunArtifact>> {
        let conn = lock_conn!(self);
        conn.query_row(
            "SELECT id, run_id, flow_name, stage_name, attempt, version, content, content_hash, created_at
             FROM artifacts WHERE id = ?1",
            params![id],
            |row| {
                Ok(RunArtifact {
                    id: row.get(0)?,
                    run_id: RunId(row.get(1)?),
                    flow_name: row.get(2)?,
                    stage_name: row.get(3)?,
                    attempt: row.get(4)?,
                    version: row.get(5)?,
                    content: row.get(6)?,
                    content_hash: row.get(7)?,
                    created_at: row.get(8)?,
                })
            },
        )
        .optional()
        .context("Failed to load artifact")
    }
}
