//! Immutable requirements packet revisions, gate bindings and reviews
//! (TJ-020/TJ-021).
//!
//! Packet revisions are append-only. Their content hash is computed from the
//! validated packet document. A gate binding names the exact revision/hash a
//! reviewer is looking at; creating another revision while that gate is open
//! atomically rebinds the gate and bumps the run state version, so a stale
//! approval conflicts instead of silently approving newer content.

use super::*;
use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use tj_packet::RequirementsPacket;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PacketRevision {
    pub id: String,
    pub packet_id: String,
    pub run_id: String,
    pub revision: i64,
    pub schema_version: i64,
    pub content_json: String,
    pub content_hash: String,
    pub completeness: String,
    pub parent_revision_id: Option<String>,
    pub source_artifact_id: Option<String>,
    pub source_artifact_hash: Option<String>,
    pub flow_name: Option<String>,
    pub stage_name: Option<String>,
    pub change_request: Option<String>,
    pub created_by: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PacketReview {
    pub id: String,
    pub packet_id: String,
    pub revision_id: String,
    pub revision: i64,
    pub content_hash: String,
    pub gate_instance_id: String,
    pub decision_id: String,
    pub actor_id: String,
    pub actor_name: String,
    pub outcome: String,
    pub reason: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatePacketBinding {
    pub gate_instance_id: String,
    pub run_id: String,
    pub packet_id: String,
    pub revision_id: String,
    pub revision: i64,
    pub content_hash: String,
    pub completeness: String,
    pub bound_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct NewPacketRevision {
    pub run_id: String,
    pub packet_id: Option<String>,
    pub content_json: String,
    pub parent_revision_id: Option<String>,
    pub source_artifact_id: Option<String>,
    pub source_artifact_hash: Option<String>,
    pub flow_name: Option<String>,
    pub stage_name: Option<String>,
    pub change_request: Option<String>,
    pub created_by: String,
    /// When present, references are checked against durable run inputs before
    /// the revision is inserted.
    pub allowed_sources: Option<HashSet<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PacketBundle {
    pub latest: Option<PacketRevision>,
    pub revisions: Vec<PacketRevision>,
    pub reviews: Vec<PacketReview>,
}

fn parse_packet(
    content_json: &str,
    allowed: Option<&HashSet<String>>,
) -> Result<RequirementsPacket> {
    let packet = RequirementsPacket::from_json(content_json)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    if let Some(allowed) = allowed {
        packet
            .validate_known_sources(allowed)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    }
    Ok(packet)
}

fn read_revision(row: &rusqlite::Row<'_>) -> rusqlite::Result<PacketRevision> {
    Ok(PacketRevision {
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
}

fn read_review(row: &rusqlite::Row<'_>) -> rusqlite::Result<PacketReview> {
    Ok(PacketReview {
        id: row.get(0)?,
        packet_id: row.get(1)?,
        revision_id: row.get(2)?,
        revision: row.get(3)?,
        content_hash: row.get(4)?,
        gate_instance_id: row.get(5)?,
        decision_id: row.get(6)?,
        actor_id: row.get(7)?,
        actor_name: row.get(8)?,
        outcome: row.get(9)?,
        reason: row.get(10)?,
        created_at: row.get(11)?,
    })
}

pub(crate) fn migrate(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS packet_revisions (
            id TEXT PRIMARY KEY,
            packet_id TEXT NOT NULL,
            run_id TEXT NOT NULL,
            revision INTEGER NOT NULL,
            schema_version INTEGER NOT NULL,
            content_json TEXT NOT NULL,
            content_hash TEXT NOT NULL,
            completeness TEXT NOT NULL,
            parent_revision_id TEXT,
            source_artifact_id TEXT,
            source_artifact_hash TEXT,
            flow_name TEXT,
            stage_name TEXT,
            change_request TEXT,
            created_by TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_packet_revisions_number
            ON packet_revisions(packet_id, revision);
        CREATE INDEX IF NOT EXISTS idx_packet_revisions_run
            ON packet_revisions(run_id, revision);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_packet_revisions_run_hash
            ON packet_revisions(run_id, content_hash);

        CREATE TABLE IF NOT EXISTS run_gate_packets (
            gate_instance_id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL,
            packet_id TEXT NOT NULL,
            revision_id TEXT NOT NULL,
            revision INTEGER NOT NULL,
            content_hash TEXT NOT NULL,
            completeness TEXT NOT NULL,
            bound_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_run_gate_packets_run ON run_gate_packets(run_id);

        CREATE TABLE IF NOT EXISTS packet_reviews (
            id TEXT PRIMARY KEY,
            packet_id TEXT NOT NULL,
            revision_id TEXT NOT NULL,
            revision INTEGER NOT NULL,
            content_hash TEXT NOT NULL,
            gate_instance_id TEXT NOT NULL,
            decision_id TEXT NOT NULL,
            actor_id TEXT NOT NULL,
            actor_name TEXT NOT NULL,
            outcome TEXT NOT NULL,
            reason TEXT,
            created_at TEXT NOT NULL
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_packet_reviews_decision ON packet_reviews(decision_id);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_packet_reviews_approval
            ON packet_reviews(revision_id) WHERE outcome = 'approved';
        CREATE INDEX IF NOT EXISTS idx_packet_reviews_revision ON packet_reviews(revision_id);",
    )?;
    for (column, declaration) in [
        ("packet_revision_id", "TEXT"),
        ("packet_content_hash", "TEXT"),
    ] {
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

impl Database {
    pub fn insert_packet_revision(
        &self,
        draft: &NewPacketRevision,
    ) -> Result<(PacketRevision, bool)> {
        anyhow::ensure!(
            !draft.created_by.trim().is_empty(),
            "packet revision creator is required"
        );
        let packet = parse_packet(&draft.content_json, draft.allowed_sources.as_ref())?;
        let completeness = packet.completeness();
        let content_hash = packet.content_hash();
        let normalized = serde_json::to_string(&packet)?;
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
            "Packet revisions cannot be written after the run is {run_status}"
        );
        let latest: Option<PacketRevision> = tx.query_row(
            "SELECT id, packet_id, run_id, revision, schema_version, content_json, content_hash, completeness,
                    parent_revision_id, source_artifact_id, source_artifact_hash, flow_name, stage_name,
                    change_request, created_by, created_at
             FROM packet_revisions WHERE run_id = ?1 ORDER BY revision DESC LIMIT 1",
            params![draft.run_id], read_revision,
        ).optional()?;
        if let Some(latest) = &latest {
            if draft
                .packet_id
                .as_deref()
                .is_some_and(|id| id != latest.packet_id)
            {
                anyhow::bail!("Packet id does not belong to this run's packet lineage");
            }
            let approved_latest: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM packet_reviews WHERE revision_id = ?1 AND outcome = 'approved')",
                params![latest.id], |row| row.get(0),
            )?;
            if approved_latest {
                let gate_name: Option<String> = tx.query_row(
                    "SELECT gate_name FROM pipeline_runs WHERE id = ?1 AND status = 'waiting_for_gate'",
                    params![draft.run_id], |row| row.get(0),
                ).optional()?.flatten();
                anyhow::ensure!(
                    gate_name.as_deref() == Some("gate-a"),
                    "The requirements packet was approved and is immutable while a plan is approved; request a new packet revision rather than editing under an approved plan"
                );
            }
            if latest.content_hash == content_hash {
                anyhow::ensure!(
                    draft.change_request.is_none(),
                    "Revision content is unchanged; edit the packet before requesting a new revision"
                );
                return Ok((latest.clone(), true));
            }
        }
        let packet_id = draft
            .packet_id
            .clone()
            .or_else(|| latest.as_ref().map(|item| item.packet_id.clone()))
            .unwrap_or_else(|| format!("pkt_{}", uuid::Uuid::new_v4()));
        let revision = latest.as_ref().map_or(1, |item| item.revision + 1);
        let parent_revision_id = draft
            .parent_revision_id
            .clone()
            .or_else(|| latest.as_ref().map(|item| item.id.clone()));
        let id = format!("pr_{}", uuid::Uuid::new_v4());
        let created_at = chrono::Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO packet_revisions
             (id, packet_id, run_id, revision, schema_version, content_json, content_hash, completeness,
              parent_revision_id, source_artifact_id, source_artifact_hash, flow_name, stage_name,
              change_request, created_by, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                id, packet_id, draft.run_id, revision, packet.schema_version as i64, normalized,
                content_hash, completeness.as_str(), parent_revision_id, draft.source_artifact_id,
                draft.source_artifact_hash, draft.flow_name, draft.stage_name,
                draft.change_request, draft.created_by, created_at
            ],
        )?;
        tx.execute(
            "INSERT INTO events (id, run_id, kind, flow_name, stage_name, summary, payload, created_at)
             VALUES (?1, ?2, 'packet_revision', ?3, ?4, ?5, ?6, ?7)",
            params![
                format!("evt_{}", uuid::Uuid::new_v4()), draft.run_id, draft.flow_name, draft.stage_name,
                draft.change_request.as_deref().unwrap_or("generated requirements packet"),
                serde_json::json!({
                    "packet_id": packet_id, "revision_id": id, "revision": revision,
                    "content_hash": content_hash, "completeness": completeness.as_str(),
                    "parent_revision_id": parent_revision_id, "change_request": draft.change_request,
                    "created_by": draft.created_by
                }).to_string(), created_at
            ],
        )?;
        // A new revision while a gate is open rebinds that review to the new
        // exact content and increments the run version. Any approval prepared
        // against the previous revision now fails the optimistic/stale check.
        let rebind = tx.execute(
            "UPDATE run_gate_packets
             SET packet_id = ?1, revision_id = ?2, revision = ?3, content_hash = ?4,
                 completeness = ?5, updated_at = ?6
             WHERE run_id = ?7 AND gate_instance_id = (
                 SELECT gate_instance_id FROM pipeline_runs WHERE id = ?7 AND status = 'waiting_for_gate'
             )",
            params![packet_id, id, revision, content_hash, completeness.as_str(), created_at, draft.run_id],
        )?;
        if rebind > 0 {
            tx.execute(
                "UPDATE pipeline_runs SET revision = revision + 1, updated_at = ?2
                 WHERE id = ?1 AND status = 'waiting_for_gate'",
                params![draft.run_id, created_at],
            )?;
        }
        let inserted = tx.query_row(
            "SELECT id, packet_id, run_id, revision, schema_version, content_json, content_hash, completeness,
                    parent_revision_id, source_artifact_id, source_artifact_hash, flow_name, stage_name,
                    change_request, created_by, created_at
             FROM packet_revisions WHERE id = ?1",
            params![id], read_revision,
        )?;
        tx.commit()?;
        Ok((inserted, false))
    }

    pub fn packet_revision(&self, revision_id: &str) -> Result<Option<PacketRevision>> {
        let conn = lock_conn!(self);
        conn.query_row(
            "SELECT id, packet_id, run_id, revision, schema_version, content_json, content_hash, completeness,
                    parent_revision_id, source_artifact_id, source_artifact_hash, flow_name, stage_name,
                    change_request, created_by, created_at
             FROM packet_revisions WHERE id = ?1",
            params![revision_id], read_revision,
        ).optional().map_err(Into::into)
    }

    /// Durable inputs a packet source may cite. Generated or reviewer-edited
    /// confirmed requirements cannot point at anything else.
    pub fn packet_allowed_sources(&self, run_id: &str) -> Result<HashSet<String>> {
        let conn = lock_conn!(self);
        let mut allowed = HashSet::new();
        allowed.insert(format!("run:{run_id}"));
        let mut artifacts =
            conn.prepare("SELECT flow_name, stage_name FROM artifacts WHERE run_id = ?1")?;
        for row in artifacts.query_map(params![run_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })? {
            let (flow, stage) = row?;
            allowed.insert(format!("artifact:{flow}:{stage}"));
        }
        let mut questions =
            conn.prepare("SELECT id FROM questions WHERE run_id = ?1 AND status = 'answered'")?;
        for row in questions.query_map(params![run_id], |row| row.get::<_, String>(0))? {
            allowed.insert(format!("answer:{}", row?));
        }
        Ok(allowed)
    }

    pub fn packet_bundle(&self, run_id: &str) -> Result<PacketBundle> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(
            "SELECT id, packet_id, run_id, revision, schema_version, content_json, content_hash, completeness,
                    parent_revision_id, source_artifact_id, source_artifact_hash, flow_name, stage_name,
                    change_request, created_by, created_at
             FROM packet_revisions WHERE run_id = ?1 ORDER BY revision",
        )?;
        let revisions = stmt
            .query_map(params![run_id], read_revision)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut reviews_stmt = conn.prepare(
            "SELECT id, packet_id, revision_id, revision, content_hash, gate_instance_id, decision_id,
                    actor_id, actor_name, outcome, reason, created_at
             FROM packet_reviews WHERE packet_id IN (SELECT DISTINCT packet_id FROM packet_revisions WHERE run_id = ?1)
             ORDER BY created_at, rowid",
        )?;
        let reviews = reviews_stmt
            .query_map(params![run_id], read_review)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let latest = revisions.last().cloned();
        Ok(PacketBundle {
            latest,
            revisions,
            reviews,
        })
    }

    pub fn gate_packet_binding(&self, gate_instance_id: &str) -> Result<Option<GatePacketBinding>> {
        let conn = lock_conn!(self);
        conn.query_row(
            "SELECT gate_instance_id, run_id, packet_id, revision_id, revision, content_hash,
                    completeness, bound_at, updated_at
             FROM run_gate_packets WHERE gate_instance_id = ?1",
            params![gate_instance_id],
            |row| {
                Ok(GatePacketBinding {
                    gate_instance_id: row.get(0)?,
                    run_id: row.get(1)?,
                    packet_id: row.get(2)?,
                    revision_id: row.get(3)?,
                    revision: row.get(4)?,
                    content_hash: row.get(5)?,
                    completeness: row.get(6)?,
                    bound_at: row.get(7)?,
                    updated_at: row.get(8)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }
}

pub(crate) fn bind_gate_packet_tx(
    tx: &Transaction<'_>,
    run_id: &str,
    gate_instance_id: &str,
    revision: &PacketRevision,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    tx.execute(
        "INSERT INTO run_gate_packets
         (gate_instance_id, run_id, packet_id, revision_id, revision, content_hash, completeness, bound_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
         ON CONFLICT(gate_instance_id) DO UPDATE SET
            packet_id=excluded.packet_id, revision_id=excluded.revision_id, revision=excluded.revision,
            content_hash=excluded.content_hash, completeness=excluded.completeness, updated_at=excluded.updated_at",
        params![
            gate_instance_id, run_id, revision.packet_id, revision.id, revision.revision,
            revision.content_hash, revision.completeness, now
        ],
    )?;
    Ok(())
}

pub(crate) fn insert_packet_review_tx(
    tx: &Transaction<'_>,
    decision: &GateDecision,
    decision_id: &str,
    binding: &GatePacketBinding,
    review_id: &str,
    created_at: &str,
) -> Result<()> {
    tx.execute(
        "INSERT INTO packet_reviews
         (id, packet_id, revision_id, revision, content_hash, gate_instance_id, decision_id,
          actor_id, actor_name, outcome, reason, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            review_id,
            binding.packet_id,
            binding.revision_id,
            binding.revision,
            binding.content_hash,
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

    fn db() -> (Database, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("tj-packets-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Database::open(&dir.join("state.db").to_string_lossy()).unwrap();
        db.save_run(&PipelineRun::new(
            "run".into(),
            None,
            None,
            "artifact-a".into(),
            RunStatus::Running,
            None,
            None,
            Some("trace".into()),
            None,
        ))
        .unwrap();
        (db, dir)
    }

    fn draft(content: &str) -> NewPacketRevision {
        NewPacketRevision {
            run_id: "run".into(),
            packet_id: None,
            content_json: content.into(),
            parent_revision_id: None,
            source_artifact_id: Some("art_intake".into()),
            source_artifact_hash: Some("hash".into()),
            flow_name: Some("flow-a".into()),
            stage_name: Some("artifact-a".into()),
            change_request: None,
            created_by: "system:packet-generation".into(),
            allowed_sources: None,
        }
    }

    #[test]
    fn revisions_are_immutable_hash_bound_and_idempotent_by_content() {
        let (db, _dir) = db();
        let first = draft(include_str!("../../tj-packet/fixtures/complete.json"));
        let (revision, replay) = db.insert_packet_revision(&first).unwrap();
        assert!(!replay);
        assert_eq!(revision.revision, 1);
        assert_eq!(revision.completeness, "complete");
        let (again, replay) = db.insert_packet_revision(&first).unwrap();
        assert!(replay);
        assert_eq!(again.id, revision.id);
        assert_eq!(db.packet_bundle("run").unwrap().revisions.len(), 1);

        let mut changed: serde_json::Value =
            serde_json::from_str(include_str!("../../tj-packet/fixtures/complete.json")).unwrap();
        changed["title"] = serde_json::json!("Changed title");
        let (second, replay) = db
            .insert_packet_revision(&draft(&changed.to_string()))
            .unwrap();
        assert!(!replay);
        assert_eq!(second.revision, 2);
        assert_eq!(
            second.parent_revision_id.as_deref(),
            Some(revision.id.as_str())
        );
        assert_ne!(second.content_hash, revision.content_hash);
    }

    #[test]
    fn new_revision_during_gate_rebinds_and_bumps_run_version() {
        let (db, _dir) = db();
        let (first, _) = db
            .insert_packet_revision(&draft(include_str!(
                "../../tj-packet/fixtures/complete.json"
            )))
            .unwrap();
        let version = db
            .enter_gate_bound("run", 0, "gate_1", "gate-a", "review", Some(&first))
            .unwrap();
        let binding = db.gate_packet_binding("gate_1").unwrap().unwrap();
        assert_eq!(binding.revision_id, first.id);
        let before = db.load_run("run").unwrap().unwrap().state_version();

        let mut changed: serde_json::Value =
            serde_json::from_str(include_str!("../../tj-packet/fixtures/complete.json")).unwrap();
        changed["title"] = serde_json::json!("Reviewer revision");
        let (second, _) = db
            .insert_packet_revision(&draft(&changed.to_string()))
            .unwrap();
        let after = db.load_run("run").unwrap().unwrap().state_version();
        assert_eq!(after, before + 1);
        assert_eq!(
            db.gate_packet_binding("gate_1")
                .unwrap()
                .unwrap()
                .revision_id,
            second.id
        );
        assert!(version < after);
    }
    fn seed_member(db: &Database) {
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
    }

    fn gate_decision(key: &str, approved: bool, revision: &PacketRevision) -> GateDecision {
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
                Some("changes needed".into())
            },
            note: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            gate_instance_id: Some("gate_1".into()),
            idempotency_key: Some(key.into()),
            expected_state_version: Some(1),
            state_version: None,
            disposition: None,
            packet_revision_id: Some(revision.id.clone()),
            packet_content_hash: Some(revision.content_hash.clone()),
            plan_revision_id: None,
            plan_content_hash: None,
            output_artifact_id: None,
            output_content_hash: None,
            output_stage_name: None,
            acknowledge_incomplete: false,
        }
    }

    #[test]
    fn approval_records_the_exact_revision_hash_actor_and_time() {
        let (db, _dir) = db();
        db.save_project(&Project {
            id: "project".into(),
            name: "project".into(),
            repo_url: None,
            base_branch: "main".into(),
            workspace_path: None,
            git_provider: None,
            profile: "flow-a".into(),
            workspace_kind: None,
            catalog_id: None,
            provisioning_state: None,
            provisioning_error: None,
            created_at: String::new(),
            updated_at: String::new(),
        })
        .unwrap();
        seed_member(&db);
        let (revision, _) = db
            .insert_packet_revision(&draft(include_str!(
                "../../tj-packet/fixtures/complete.json"
            )))
            .unwrap();
        let version = db
            .enter_gate_bound("run", 0, "gate_1", "gate-a", "review", Some(&revision))
            .unwrap();
        let good = gate_decision("approval", true, &revision);
        let receipt = db.decide_gate(&good, version, 3).unwrap();
        assert!(!receipt.replayed);
        assert_eq!(
            receipt.packet_revision_id.as_deref(),
            Some(revision.id.as_str())
        );
        assert_eq!(
            receipt.packet_content_hash.as_deref(),
            Some(revision.content_hash.as_str())
        );
        let bundle = db.packet_bundle("run").unwrap();
        assert_eq!(bundle.reviews.len(), 1);
        assert_eq!(bundle.reviews[0].outcome, "approved");
        assert_eq!(bundle.reviews[0].content_hash, revision.content_hash);
        assert_eq!(bundle.reviews[0].actor_name, "Alice");
        let decisions = db.list_gate_decisions("run").unwrap();
        assert_eq!(
            decisions[0].packet_revision_id.as_deref(),
            Some(revision.id.as_str())
        );
        assert_eq!(
            decisions[0].packet_content_hash.as_deref(),
            Some(revision.content_hash.as_str())
        );
    }

    #[test]
    fn stale_packet_hash_conflicts_before_any_review_or_decision_is_written() {
        let (db, _dir) = db();
        db.save_project(&Project {
            id: "project".into(),
            name: "project".into(),
            repo_url: None,
            base_branch: "main".into(),
            workspace_path: None,
            git_provider: None,
            profile: "flow-a".into(),
            workspace_kind: None,
            catalog_id: None,
            provisioning_state: None,
            provisioning_error: None,
            created_at: String::new(),
            updated_at: String::new(),
        })
        .unwrap();
        seed_member(&db);
        let (revision, _) = db
            .insert_packet_revision(&draft(include_str!(
                "../../tj-packet/fixtures/complete.json"
            )))
            .unwrap();
        let version = db
            .enter_gate_bound("run", 0, "gate_1", "gate-a", "review", Some(&revision))
            .unwrap();
        let mut stale = gate_decision("stale-approval", true, &revision);
        stale.packet_content_hash = Some("f".repeat(64));
        let error = db.decide_gate(&stale, version, 3).unwrap_err().to_string();
        assert!(error.contains("stale or missing"), "{error}");
        assert!(db.list_gate_decisions("run").unwrap().is_empty());
        assert!(db.packet_bundle("run").unwrap().reviews.is_empty());
        assert_eq!(
            db.load_run("run").unwrap().unwrap().status(),
            &RunStatus::WaitingForGate
        );

        let mut replay_mismatch = gate_decision("stale-approval", true, &revision);
        replay_mismatch.packet_revision_id = Some("another_revision".into());
        let error = db
            .decide_gate(&replay_mismatch, version, 3)
            .unwrap_err()
            .to_string();
        assert!(error.contains("stale or missing"), "{error}");
    }

    #[test]
    fn rejected_review_retains_revision_cause_and_creates_no_approval() {
        let (db, _dir) = db();
        db.save_project(&Project {
            id: "project".into(),
            name: "project".into(),
            repo_url: None,
            base_branch: "main".into(),
            workspace_path: None,
            git_provider: None,
            profile: "flow-a".into(),
            workspace_kind: None,
            catalog_id: None,
            provisioning_state: None,
            provisioning_error: None,
            created_at: String::new(),
            updated_at: String::new(),
        })
        .unwrap();
        seed_member(&db);
        let (revision, _) = db
            .insert_packet_revision(&draft(include_str!(
                "../../tj-packet/fixtures/incomplete.json"
            )))
            .unwrap();
        let version = db
            .enter_gate_bound("run", 0, "gate_1", "gate-a", "review", Some(&revision))
            .unwrap();
        let rejection = gate_decision("reject-1", false, &revision);
        let receipt = db.decide_gate(&rejection, version, 3).unwrap();
        assert_eq!(receipt.disposition, "rework");
        let bundle = db.packet_bundle("run").unwrap();
        assert_eq!(bundle.revisions.len(), 1);
        assert_eq!(bundle.reviews.len(), 1);
        assert_eq!(bundle.reviews[0].revision_id, revision.id);
        assert_eq!(bundle.reviews[0].outcome, "changes_requested");
        assert_eq!(bundle.reviews[0].reason.as_deref(), Some("changes needed"));
        assert_eq!(bundle.reviews[0].actor_name, "Alice");
        assert_eq!(bundle.reviews[0].content_hash, revision.content_hash);
        assert!(
            !bundle
                .reviews
                .iter()
                .any(|review| review.outcome == "approved")
        );
        let decisions = db.list_gate_decisions("run").unwrap();
        assert_eq!(decisions.len(), 1);
        assert!(!decisions[0].approved);
        assert_eq!(
            decisions[0].packet_revision_id.as_deref(),
            Some(revision.id.as_str())
        );
    }

    #[test]
    fn incomplete_packet_approval_requires_explicit_acknowledgement() {
        let (db, _dir) = db();
        db.save_project(&Project {
            id: "project".into(),
            name: "project".into(),
            repo_url: None,
            base_branch: "main".into(),
            workspace_path: None,
            git_provider: None,
            profile: "flow-a".into(),
            workspace_kind: None,
            catalog_id: None,
            provisioning_state: None,
            provisioning_error: None,
            created_at: String::new(),
            updated_at: String::new(),
        })
        .unwrap();
        seed_member(&db);
        let (revision, _) = db
            .insert_packet_revision(&draft(include_str!(
                "../../tj-packet/fixtures/incomplete.json"
            )))
            .unwrap();
        let version = db
            .enter_gate_bound("run", 0, "gate_1", "gate-a", "review", Some(&revision))
            .unwrap();
        let mut approval = gate_decision("incomplete-approval", true, &revision);
        let error = db
            .decide_gate(&approval, version, 3)
            .unwrap_err()
            .to_string();
        assert!(error.contains("explicit acknowledgement"), "{error}");
        assert!(db.list_gate_decisions("run").unwrap().is_empty());
        assert_eq!(
            db.load_run("run").unwrap().unwrap().status(),
            &RunStatus::WaitingForGate
        );
        approval.acknowledge_incomplete = true;
        let receipt = db.decide_gate(&approval, version, 3).unwrap();
        assert_eq!(receipt.disposition, "approved");
        assert_eq!(db.list_gate_decisions("run").unwrap().len(), 1);
    }
}
