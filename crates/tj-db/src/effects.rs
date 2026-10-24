//! Durable pending-effect state (TJ-017).
//!
//! State transitions that can have follow-on work are recorded before the
//! follow-on starts. A crash between a durable acceptance and the manager's
//! routing therefore leaves an inspectable row. Pure routing effects (a gate
//! decision moving to the next flow) can be reconciled safely because no
//! model/tool/publication work has started; effects that may have touched an
//! external system are marked ambiguous and require explicit reconciliation.

use super::*;
use rusqlite::Transaction;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectStatus {
    Pending,
    Running,
    Completed,
    Ambiguous,
    Failed,
}

impl EffectStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            EffectStatus::Pending => "pending",
            EffectStatus::Running => "running",
            EffectStatus::Completed => "completed",
            EffectStatus::Ambiguous => "ambiguous",
            EffectStatus::Failed => "failed",
        }
    }
}

impl FromStr for EffectStatus {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "pending" => Ok(EffectStatus::Pending),
            "running" => Ok(EffectStatus::Running),
            "completed" => Ok(EffectStatus::Completed),
            "ambiguous" => Ok(EffectStatus::Ambiguous),
            "failed" => Ok(EffectStatus::Failed),
            other => Err(format!("Unknown EffectStatus: {other}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingEffect {
    pub id: String,
    pub run_id: String,
    pub kind: String,
    pub status: EffectStatus,
    pub idempotency_key: String,
    pub payload: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
    pub error: Option<String>,
}

const EFFECT_SELECT: &str = "SELECT id, run_id, kind, status, idempotency_key, payload, created_at, updated_at, completed_at, error FROM pending_effects";

fn read_effect(row: &rusqlite::Row<'_>) -> rusqlite::Result<PendingEffect> {
    let status: String = row.get(3)?;
    Ok(PendingEffect {
        id: row.get(0)?,
        run_id: row.get(1)?,
        kind: row.get(2)?,
        status: status.parse().unwrap_or(EffectStatus::Ambiguous),
        idempotency_key: row.get(4)?,
        payload: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
        completed_at: row.get(8)?,
        error: row.get(9)?,
    })
}

pub(super) fn insert_tx(
    tx: &Transaction<'_>,
    run_id: &str,
    kind: &str,
    idempotency_key: &str,
    payload: &serde_json::Value,
) -> Result<String> {
    let id = format!("effect_{}", uuid::Uuid::new_v4());
    let now = chrono::Utc::now().to_rfc3339();
    tx.execute(
        "INSERT INTO pending_effects (id, run_id, kind, status, idempotency_key, payload, created_at, updated_at)
         VALUES (?1, ?2, ?3, 'pending', ?4, ?5, ?6, ?6)",
        params![id, run_id, kind, idempotency_key, payload.to_string(), now],
    )?;
    Ok(id)
}

impl Database {
    pub fn create_pending_effect(
        &self,
        run_id: &str,
        kind: &str,
        idempotency_key: &str,
        payload: &serde_json::Value,
    ) -> Result<String> {
        anyhow::ensure!(
            !idempotency_key.trim().is_empty(),
            "pending effect requires an idempotency key"
        );
        let mut conn = lock_conn!(self);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let id = insert_tx(&tx, run_id, kind, idempotency_key, payload)?;
        tx.commit()?;
        Ok(id)
    }

    pub fn load_pending_effect(&self, id: &str) -> Result<Option<PendingEffect>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(&format!("{EFFECT_SELECT} WHERE id = ?1"))?;
        let mut rows = stmt.query(params![id])?;
        match rows.next()? {
            Some(row) => Ok(Some(read_effect(row)?)),
            None => Ok(None),
        }
    }

    pub fn pending_effect_by_key(&self, idempotency_key: &str) -> Result<Option<PendingEffect>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(&format!("{EFFECT_SELECT} WHERE idempotency_key = ?1"))?;
        let mut rows = stmt.query(params![idempotency_key])?;
        match rows.next()? {
            Some(row) => Ok(Some(read_effect(row)?)),
            None => Ok(None),
        }
    }

    pub fn list_pending_effects(
        &self,
        run_id: &str,
        unresolved_only: bool,
    ) -> Result<Vec<PendingEffect>> {
        let conn = lock_conn!(self);
        let sql = if unresolved_only {
            format!(
                "{EFFECT_SELECT} WHERE run_id = ?1 AND status IN ('pending','running','ambiguous') ORDER BY rowid"
            )
        } else {
            format!("{EFFECT_SELECT} WHERE run_id = ?1 ORDER BY rowid")
        };
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![run_id], read_effect)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Claim a pending effect before its follow-on starts. A running effect is
    /// not claimed twice; recovery must inspect it explicitly.
    pub fn claim_pending_effect(&self, id: &str) -> Result<bool> {
        let conn = lock_conn!(self);
        let changed = conn.execute(
            "UPDATE pending_effects SET status = 'running', updated_at = ?2
             WHERE id = ?1 AND status = 'pending'",
            params![id, chrono::Utc::now().to_rfc3339()],
        )?;
        Ok(changed == 1)
    }

    /// Recovery-only reset for a pure routing effect that was claimed but left
    /// no execution attempt. External-effect kinds must not use this.
    pub fn reopen_pending_effect(&self, id: &str) -> Result<bool> {
        let conn = lock_conn!(self);
        let changed = conn.execute(
            "UPDATE pending_effects SET status = 'pending', updated_at = ?2
             WHERE id = ?1 AND status = 'running'",
            params![id, chrono::Utc::now().to_rfc3339()],
        )?;
        Ok(changed == 1)
    }

    pub fn complete_pending_effect(&self, id: &str) -> Result<bool> {
        let conn = lock_conn!(self);
        let now = chrono::Utc::now().to_rfc3339();
        let changed = conn.execute(
            "UPDATE pending_effects SET status = 'completed', completed_at = ?2, updated_at = ?2
             WHERE id = ?1 AND status IN ('pending','running')",
            params![id, now],
        )?;
        Ok(changed == 1)
    }

    pub fn mark_pending_effect_ambiguous(&self, id: &str, reason: &str) -> Result<bool> {
        let conn = lock_conn!(self);
        let changed = conn.execute(
            "UPDATE pending_effects SET status = 'ambiguous', error = ?2, updated_at = ?3
             WHERE id = ?1 AND status IN ('pending','running')",
            params![id, reason, chrono::Utc::now().to_rfc3339()],
        )?;
        Ok(changed == 1)
    }

    pub fn fail_pending_effect(&self, id: &str, reason: &str) -> Result<bool> {
        let conn = lock_conn!(self);
        let changed = conn.execute(
            "UPDATE pending_effects SET status = 'failed', error = ?2, updated_at = ?3
             WHERE id = ?1 AND status IN ('pending','running')",
            params![id, reason, chrono::Utc::now().to_rfc3339()],
        )?;
        Ok(changed == 1)
    }

    /// True when a replacement execution attempt was durably reserved at or
    /// after `timestamp`. Used to prove a claimed route already spawned work.
    pub fn has_attempt_since(&self, run_id: &str, timestamp: &str) -> Result<bool> {
        let conn = lock_conn!(self);
        let exists: i64 = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM execution_attempts WHERE run_id = ?1 AND created_at >= ?2)",
            params![run_id, timestamp], |row| row.get(0),
        )?;
        Ok(exists != 0)
    }
}

pub(super) fn migrate(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS pending_effects (
            id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            status TEXT NOT NULL,
            idempotency_key TEXT NOT NULL,
            payload TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            completed_at TEXT,
            error TEXT
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_pending_effect_key
            ON pending_effects(idempotency_key);
        CREATE INDEX IF NOT EXISTS idx_pending_effect_run
            ON pending_effects(run_id, status);",
    )
    .context("Failed to create pending_effects table")?;
    Ok(())
}
