use super::*;
use rusqlite::{OptionalExtension, Transaction};
use std::sync::atomic::{AtomicI64, Ordering};

/// Private optimistic-lock token. Clones are independent snapshots, not shared
/// counters. Deserialized/new records are insert-only until saved or reloaded.
#[derive(Debug)]
pub(super) struct Revision(AtomicI64);

impl Default for Revision {
    fn default() -> Self {
        Self::loaded(-1)
    }
}

impl Clone for Revision {
    fn clone(&self) -> Self {
        Self::loaded(self.get())
    }
}

impl Revision {
    pub(super) fn loaded(value: i64) -> Self {
        Self(AtomicI64::new(value))
    }
    pub(super) fn get(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
    pub(super) fn advance(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

pub(super) fn serialize_revision<S: serde::Serializer>(
    revision: &Revision,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_i64(revision.get())
}

fn legal(table: &str, from: &str, to: &str) -> bool {
    match (table, from) {
        ("pipeline_runs", "running" | "waiting_for_gate") => matches!(
            to,
            "running" | "waiting_for_gate" | "interrupted" | "completed" | "failed" | "cancelled"
        ),
        ("pipeline_runs", "interrupted") => {
            matches!(to, "running" | "completed" | "failed" | "cancelled")
        }
        ("flows" | "stages", "pending") => {
            matches!(to, "pending" | "running" | "failed" | "timed_out")
        }
        ("flows", "running") => matches!(to, "running" | "completed" | "failed" | "timed_out"),
        ("stages", "running" | "waiting_for_input") => matches!(
            to,
            "running" | "waiting_for_input" | "completed" | "failed" | "timed_out"
        ),
        _ => false,
    }
}

pub(super) fn guard_write(
    tx: &Transaction<'_>,
    table: &str,
    id: &str,
    revision: &Revision,
    status: &str,
) -> Result<()> {
    let expected = revision.get();
    let existing: Option<(i64, String)> = tx
        .query_row(
            &format!("SELECT revision, status FROM {table} WHERE id = ?1"),
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match existing {
        None if expected == -1 => Ok(()),
        Some((version, from)) if version == expected && legal(table, &from, status) => {
            let changed = tx.execute(
                &format!(
                    "UPDATE {table} SET revision = revision + 1 WHERE id = ?1 AND revision = ?2"
                ),
                params![id, expected],
            )?;
            anyhow::ensure!(changed == 1, "Lifecycle conflict for {table}/{id}");
            Ok(())
        }
        _ => {
            anyhow::bail!("Lifecycle conflict for {table}/{id}: stale, missing, or terminal record")
        }
    }
}

pub(super) fn finish_write(
    tx: &Transaction<'_>,
    table: &str,
    id: &str,
    run_id: &str,
    status: &str,
    revision: &Revision,
) -> Result<()> {
    tx.execute(
        "INSERT INTO events (id, run_id, kind, payload, created_at) VALUES (?1, ?2, 'lifecycle_transition', ?3, ?4)",
        params![format!("evt_{}", uuid::Uuid::new_v4()), run_id,
            serde_json::json!({"entity": table, "id": id, "revision": revision.get() + 1, "status": status}).to_string(),
            chrono::Utc::now().to_rfc3339()],
    )?;
    if table == "pipeline_runs" && matches!(status, "completed" | "failed" | "cancelled") {
        // Fence all active descendants in the same transaction as the run.
        // Workers holding old snapshots will conflict instead of resurrecting.
        for (entity, predicate) in [
            (
                "stages",
                "flow_id IN (SELECT id FROM flows WHERE run_id = ?1) AND status IN ('pending','running','waiting_for_input')",
            ),
            ("flows", "run_id = ?1 AND status IN ('pending','running')"),
        ] {
            finalize_rows(tx, entity, run_id, "parent run ended", predicate)?;
        }
    }
    Ok(())
}

pub(super) fn finalize_rows(
    tx: &Transaction<'_>,
    entity: &str,
    run_id: &str,
    reason: &str,
    predicate: &str,
) -> Result<usize> {
    let now = chrono::Utc::now().to_rfc3339();
    tx.execute(&format!("INSERT INTO events (id, run_id, kind, payload, created_at)
        SELECT 'evt_' || lower(hex(randomblob(16))), ?1, 'lifecycle_transition',
        json_object('entity', '{entity}', 'id', id, 'revision', revision + 1, 'status', 'failed', 'reason', ?3), ?2
        FROM {entity} WHERE {predicate}"), params![run_id, now, reason])?;
    let error = if entity == "stages" {
        ", error = ?3"
    } else {
        ""
    };
    // Bind reason for both forms, without an unused SQL parameter.
    let n = tx.execute(&format!("UPDATE {entity} SET status = 'failed', revision = revision + 1, completed_at = ?2 {error} WHERE ({predicate}) AND ?3 IS NOT NULL"), params![run_id, now, reason])?;
    Ok(n)
}

pub(super) fn guard_flow_parent(tx: &Transaction<'_>, run_id: &str, flow_id: &str) -> Result<()> {
    let active: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM pipeline_runs WHERE id = ?1 AND status = 'running' AND (current_flow_id IS NULL OR current_flow_id = ?2)
        AND (deadline_at IS NULL OR julianday(deadline_at) > julianday('now')))",
        params![run_id, flow_id], |r| r.get(0))?;
    anyhow::ensure!(
        active,
        "Lifecycle conflict: flow parent is missing, terminal, or superseded"
    );
    Ok(())
}

pub(super) fn guard_stage_parent(tx: &Transaction<'_>, flow_id: &str) -> Result<()> {
    let run_id: String = tx.query_row(
        "SELECT run_id FROM flows WHERE id = ?1 AND status = 'running'",
        params![flow_id],
        |r| r.get(0),
    )?;
    guard_flow_parent(tx, &run_id, flow_id)
}

pub(super) fn migrate(conn: &rusqlite::Connection) -> Result<()> {
    for table in ["pipeline_runs", "flows", "stages"] {
        if conn
            .prepare(&format!("SELECT revision FROM {table} LIMIT 0"))
            .is_err()
        {
            conn.execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN revision INTEGER NOT NULL DEFAULT 0;"
            ))?;
        }
    }
    if conn
        .prepare("SELECT deadline_at FROM pipeline_runs LIMIT 0")
        .is_err()
    {
        conn.execute_batch("ALTER TABLE pipeline_runs ADD COLUMN deadline_at TEXT;")?;
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS execution_attempts (
        id TEXT PRIMARY KEY, run_id TEXT NOT NULL, flow_id TEXT NOT NULL,
        flow_name TEXT NOT NULL, consumed INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL
    ); CREATE INDEX IF NOT EXISTS execution_attempt_run ON execution_attempts(run_id);",
    )?;
    Ok(())
}

impl Database {
    /// Startup-only fencing: the previous process owns no live worker in this
    /// process. Persist abandonment before scheduling its replacement attempt.
    pub fn abandon_execution_on_restart(&self, run_id: &str) -> Result<()> {
        let mut conn = lock_conn!(self);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE execution_attempts SET consumed = 1 WHERE run_id = ?1 AND consumed = 0",
            params![run_id],
        )?;
        attempt_event(&tx, run_id, "restart", "execution_abandoned_on_restart")?;
        tx.commit()?;
        Ok(())
    }

    /// Reserve an immutable flow/attempt identity before starting asynchronous
    /// work. Duplicate requests cannot replace an unconsumed active attempt.
    pub fn begin_flow_attempt(
        &self,
        run_id: &str,
        flow_name: &str,
        flow_id: &str,
    ) -> Result<String> {
        let mut conn = lock_conn!(self);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let id = format!("attempt_{}", uuid::Uuid::new_v4());
        let changed = tx.execute(
            "UPDATE pipeline_runs SET current_flow_id = ?2, revision = revision + 1, updated_at = ?3
             WHERE id = ?1 AND status = 'running'
             AND (deadline_at IS NULL OR julianday(deadline_at) > julianday('now'))
             AND NOT EXISTS (SELECT 1 FROM execution_attempts WHERE run_id = ?1 AND consumed = 0)",
            params![run_id, flow_id, chrono::Utc::now().to_rfc3339()],
        )?;
        anyhow::ensure!(changed == 1, "Run cannot start another execution attempt");
        finalize_rows(
            &tx,
            "stages",
            run_id,
            "execution superseded",
            "status IN ('pending','running','waiting_for_input') AND flow_id IN (SELECT f.id FROM flows f JOIN pipeline_runs r ON r.id = f.run_id WHERE r.id = ?1 AND f.id != r.current_flow_id)",
        )?;
        finalize_rows(
            &tx,
            "flows",
            run_id,
            "execution superseded",
            "run_id = ?1 AND status IN ('pending','running') AND id != (SELECT current_flow_id FROM pipeline_runs WHERE id = ?1)",
        )?;
        tx.execute("INSERT INTO execution_attempts (id, run_id, flow_id, flow_name, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, run_id, flow_id, flow_name, chrono::Utc::now().to_rfc3339()])?;
        attempt_event(&tx, run_id, &id, "execution_started")?;
        tx.commit()?;
        Ok(id)
    }

    /// Durable at-most-once acceptance before routing or remote effects. A
    /// crash after acceptance requires explicit recovery, never blind replay.
    pub fn accept_flow_completion(
        &self,
        run_id: &str,
        flow_name: &str,
        attempt_id: &str,
    ) -> Result<bool> {
        let mut conn = lock_conn!(self);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE execution_attempts SET consumed = 1 WHERE id = ?1 AND run_id = ?2 AND flow_name = ?3 AND consumed = 0
             AND EXISTS (SELECT 1 FROM pipeline_runs r WHERE r.id = ?2 AND r.status = 'running' AND r.current_flow_id = execution_attempts.flow_id
                 AND (r.deadline_at IS NULL OR julianday(r.deadline_at) > julianday('now')))",
            params![attempt_id, run_id, flow_name],
        )?;
        if changed == 0 {
            return Ok(false);
        }
        attempt_event(&tx, run_id, attempt_id, "execution_completion_accepted")?;
        // Durable follow-on intent: publication/route must not be guessed from
        // in-memory state after a crash.
        effects::insert_tx(
            &tx,
            run_id,
            "flow_completion",
            &format!("flow_completion:{attempt_id}"),
            &serde_json::json!({"run_id": run_id, "flow_name": flow_name, "attempt_id": attempt_id}),
        )?;
        tx.commit()?;
        Ok(true)
    }
}

fn attempt_event(tx: &Transaction<'_>, run_id: &str, attempt: &str, kind: &str) -> Result<()> {
    tx.execute(
        "INSERT INTO events (id, run_id, kind, payload, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            format!("evt_{}", uuid::Uuid::new_v4()),
            run_id,
            kind,
            serde_json::json!({"attempt_id": attempt}).to_string(),
            chrono::Utc::now().to_rfc3339()
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
