//! Interrupted-run reconciliation state (TJ-017).
//!
//! A process that starts with a `running` row cannot know whether a model,
//! tool or publication action completed. Rather than replaying it, startup
//! fences the execution and records an explicit interrupted/recovery row. A
//! retry is a separate, explicit command that starts a new attempt while the
//! durable answers and artifacts stay in place.

use super::*;
use rusqlite::{OptionalExtension, params};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryState {
    pub run_id: String,
    pub interrupted_at: String,
    pub reason: String,
    pub resume_flow: Option<String>,
    pub resume_stage: Option<String>,
    /// True when an accepted completion or publication effect may have touched
    /// an external system before the crash. A retry must acknowledge this.
    pub effects_unknown: bool,
    pub retry_count: u32,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPlan {
    pub run_id: String,
    pub flow_name: String,
    pub attempt_offset: u32,
    pub effects_unknown_acknowledged: bool,
}

fn read_recovery(row: &rusqlite::Row<'_>) -> rusqlite::Result<RecoveryState> {
    Ok(RecoveryState {
        run_id: row.get(0)?,
        interrupted_at: row.get(1)?,
        reason: row.get(2)?,
        resume_flow: row.get(3)?,
        resume_stage: row.get(4)?,
        effects_unknown: row.get::<_, i64>(5)? != 0,
        retry_count: u32::try_from(row.get::<_, i64>(6)?.max(0)).unwrap_or(u32::MAX),
        updated_at: row.get(7)?,
    })
}

impl Database {
    /// Fence a run left `running` by a previous process. Accepted answers and
    /// artifacts are untouched; open questions remain visible but cannot be
    /// answered until an explicit retry starts a new attempt.
    pub fn mark_run_interrupted(
        &self,
        run_id: &str,
        reason: &str,
        effects_unknown: bool,
    ) -> Result<Option<RecoveryState>> {
        let mut conn = lock_conn!(self);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current: Option<(String, Option<String>, i64)> = tx
            .query_row(
                "SELECT status, current_flow_id, revision FROM pipeline_runs WHERE id = ?1",
                params![run_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((status, current_flow_id, revision)) = current else {
            return Ok(None);
        };
        if status != "running" {
            return Ok(None);
        }
        let resume_flow: Option<String> = tx
            .query_row(
                "SELECT flow_name FROM flows WHERE run_id = ?1 ORDER BY rowid DESC LIMIT 1",
                params![run_id],
                |row| row.get(0),
            )
            .optional()?;
        let resume_flow = resume_flow.or_else(|| {
            current_flow_id.and_then(|id| {
                tx.query_row(
                    "SELECT flow_name FROM flows WHERE id = ?1",
                    params![id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .ok()
                .flatten()
            })
        });
        let resume_stage: Option<String> = tx
            .query_row(
                "SELECT s.stage_name FROM stages s JOIN flows f ON f.id = s.flow_id
             WHERE f.run_id = ?1 ORDER BY s.rowid DESC LIMIT 1",
                params![run_id],
                |row| row.get(0),
            )
            .optional()?;

        let rev = lifecycle::Revision::loaded(revision);
        lifecycle::guard_write(&tx, "pipeline_runs", run_id, &rev, "interrupted")?;
        let now = chrono::Utc::now().to_rfc3339();
        let changed = tx.execute(
            "UPDATE pipeline_runs SET status = 'interrupted', revision = revision + 1, updated_at = ?2
             WHERE id = ?1 AND revision = ?3 AND status = 'running'",
            params![run_id, now, revision + 1],
        )?;
        anyhow::ensure!(
            changed == 1,
            "Interrupted transition conflict for run '{run_id}'"
        );
        // The previous process owns no live worker. Fence descendants and
        // attempts, but keep durable answers/artifacts and open questions.
        lifecycle::finalize_rows(
            &tx,
            "stages",
            run_id,
            "interrupted by process restart; explicit retry required",
            "status IN ('pending','running','waiting_for_input')",
        )?;
        lifecycle::finalize_rows(
            &tx,
            "flows",
            run_id,
            "interrupted by process restart; explicit retry required",
            "status IN ('pending','running')",
        )?;
        tx.execute(
            "UPDATE execution_attempts SET consumed = 1 WHERE run_id = ?1 AND consumed = 0",
            params![run_id],
        )?;
        let attempt = format!("restart_{run_id}");
        tx.execute(
            "INSERT INTO events (id, run_id, kind, payload, created_at) VALUES (?1, ?2, 'run_interrupted', ?3, ?4)",
            params![format!("evt_{}", uuid::Uuid::new_v4()), run_id,
                serde_json::json!({"reason":reason,"effects_unknown":effects_unknown,
                    "resume_flow":resume_flow,"resume_stage":resume_stage}).to_string(), now],
        )?;
        let _ = attempt;
        tx.execute(
            "INSERT INTO run_recovery (run_id, interrupted_at, reason, resume_flow, resume_stage, effects_unknown, retry_count, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?2)
             ON CONFLICT(run_id) DO UPDATE SET interrupted_at=excluded.interrupted_at,
                 reason=excluded.reason, resume_flow=excluded.resume_flow, resume_stage=excluded.resume_stage,
                 effects_unknown=excluded.effects_unknown, updated_at=excluded.updated_at",
            params![run_id, now, reason, resume_flow, resume_stage, effects_unknown as i64],
        )?;
        tx.commit()?;
        drop(conn);
        if self.has_run_budget(run_id).unwrap_or(false) {
            let _ = self.reconcile_budget_intervals(run_id, chrono::Utc::now(), "run interrupted");
        }
        self.recovery_state(run_id)
    }

    pub fn recovery_state(&self, run_id: &str) -> Result<Option<RecoveryState>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(
            "SELECT run_id, interrupted_at, reason, resume_flow, resume_stage, effects_unknown, retry_count, updated_at
             FROM run_recovery WHERE run_id = ?1",
        )?;
        let mut rows = stmt.query(params![run_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(read_recovery(row)?)),
            None => Ok(None),
        }
    }

    /// Explicit safe retry: interrupted -> running with a durable plan. The
    /// caller must pass `acknowledge_unknown_effects` when reconciliation says
    /// an external effect may have run.
    pub fn retry_interrupted_run(
        &self,
        run_id: &str,
        acknowledge_unknown_effects: bool,
    ) -> Result<RetryPlan> {
        let mut conn = lock_conn!(self);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current: Option<(String, i64)> = tx
            .query_row(
                "SELECT status, revision FROM pipeline_runs WHERE id = ?1",
                params![run_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((status, revision)) = current else {
            anyhow::bail!("Run not found");
        };
        anyhow::ensure!(
            status == "interrupted",
            "Run '{}' is {}; only interrupted runs can be retried",
            run_id,
            status
        );
        let recovery = tx.query_row(
            "SELECT resume_flow, effects_unknown, retry_count FROM run_recovery WHERE run_id = ?1",
            params![run_id], |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)? != 0, row.get::<_, i64>(2)?)),
        ).optional()?;
        let (resume_flow, effects_unknown, retry_count) = recovery.unwrap_or((None, false, 0));
        anyhow::ensure!(
            !effects_unknown || acknowledge_unknown_effects,
            "Run has unresolved external effects from the interruption; inspect events/effects and retry with acknowledge_unknown_effects=true"
        );
        let flow_name = resume_flow
            .or_else(|| {
                tx.query_row(
                    "SELECT flow_name FROM flows WHERE run_id = ?1 ORDER BY rowid DESC LIMIT 1",
                    params![run_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .ok()
                .flatten()
            })
            .ok_or_else(|| anyhow::anyhow!("Run '{}' has no flow to retry", run_id))?;
        let rev = lifecycle::Revision::loaded(revision);
        lifecycle::guard_write(&tx, "pipeline_runs", run_id, &rev, "running")?;
        let now = chrono::Utc::now().to_rfc3339();
        let changed = tx.execute(
            "UPDATE pipeline_runs SET status = 'running', revision = revision + 1, updated_at = ?2
             WHERE id = ?1 AND revision = ?3 AND status = 'interrupted'",
            params![run_id, now, revision + 1],
        )?;
        anyhow::ensure!(changed == 1, "Retry transition conflict for run '{run_id}'");
        // A new attempt cannot reuse an old question identity; close the old
        // open questions visibly. Accepted answers are never rewritten.
        questions::close_open_for_run_tx(
            &tx,
            run_id,
            "run interrupted; explicit retry started a new attempt",
        )?;
        let attempt_offset = u32::try_from(retry_count.max(0)).unwrap_or(0) + 1;
        tx.execute(
            "UPDATE run_recovery SET retry_count = retry_count + 1, effects_unknown = 0, updated_at = ?2
             WHERE run_id = ?1",
            params![run_id, now],
        )?;
        tx.execute(
            "INSERT INTO events (id, run_id, kind, payload, created_at) VALUES (?1, ?2, 'retry_started', ?3, ?4)",
            params![format!("evt_{}", uuid::Uuid::new_v4()), run_id,
                serde_json::json!({"flow":flow_name,"attempt_offset":attempt_offset,
                    "acknowledged_unknown_effects":acknowledge_unknown_effects}).to_string(), now],
        )?;
        tx.commit()?;
        Ok(RetryPlan {
            run_id: run_id.to_string(),
            flow_name,
            attempt_offset,
            effects_unknown_acknowledged: effects_unknown,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed() -> (Database, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("tj-recovery-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Database::open(&dir.join("state.db").to_string_lossy()).unwrap();
        db.save_run(&PipelineRun::new(
            "run".into(),
            None,
            None,
            "recovery".into(),
            RunStatus::Running,
            None,
            None,
            Some("trace".into()),
            None,
        ))
        .unwrap();
        db.save_flow(&FlowRecord::new(
            "flow".into(),
            RunId("run".into()),
            "flow-a".into(),
            Some(1),
            None,
            None,
        ))
        .unwrap();
        let mut stage = StageRecord::new(
            "stage".into(),
            FlowId("flow".into()),
            "stage-a".into(),
            StageStatus::Running,
            0,
            None,
        );
        db.save_stage(&stage).unwrap();
        stage.set_status(StageStatus::WaitingForInput);
        db.save_stage(&stage).unwrap();
        db.create_question(&QuestionRecord::new(
            "question".into(),
            RunId("run".into()),
            "flow".into(),
            "stage".into(),
            "stage-a".into(),
            "flow-a".into(),
            0,
            r#"{"question":"Q?","options":[],"multi_select":false,"free_form":true}"#.into(),
        ))
        .unwrap();
        (db, dir)
    }

    #[test]
    fn restart_marks_interrupted_without_replaying_and_retry_is_explicit() {
        let (db, dir) = seed();
        db.init_run_budget(
            "run",
            &RunBudgetLimits {
                question_rounds_per_stage: 6,
                question_rounds_per_run: 12,
                model_calls: 5,
                tool_calls: 5,
                output_corrections: 2,
                interaction_nudges: 2,
                active_ms: 60_000,
                cost_micros: 0,
            },
        )
        .unwrap();
        db.charge_budget("run", BUDGET_MODEL_CALLS, Some("flow-a:stage-a"), "model-1")
            .unwrap();
        db.start_budget_interval("run", "attempt-before-crash")
            .unwrap();
        let effect = db
            .create_pending_effect(
                "run",
                "flow_completion",
                "flow_completion:attempt",
                &serde_json::json!({"flow":"flow-a"}),
            )
            .unwrap();
        let recovery = db
            .mark_run_interrupted("run", "process restart", true)
            .unwrap()
            .unwrap();
        assert!(recovery.effects_unknown);
        assert_eq!(recovery.resume_flow.as_deref(), Some("flow-a"));
        assert_eq!(
            *db.load_run("run").unwrap().unwrap().status(),
            RunStatus::Interrupted
        );
        // Pending question stays visible; stale attempt is fenced.
        assert_eq!(db.list_questions_for_run("run", true).unwrap().len(), 1);
        assert!(
            db.accept_flow_completion("run", "flow-a", "old")
                .unwrap_or(false)
                == false
        );
        assert_eq!(
            db.load_pending_effect(&effect).unwrap().unwrap().status,
            EffectStatus::Pending
        );
        // Retry without acknowledging unknown effects is refused.
        let error = db
            .retry_interrupted_run("run", false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("external effects"), "{error}");
        let plan = db.retry_interrupted_run("run", true).unwrap();
        assert_eq!(plan.flow_name, "flow-a");
        assert_eq!(plan.attempt_offset, 1);
        assert_eq!(
            *db.load_run("run").unwrap().unwrap().status(),
            RunStatus::Running
        );
        assert!(
            db.list_questions_for_run("run", true).unwrap().is_empty(),
            "old question rows stay durable but are no longer open"
        );
        assert_eq!(db.recovery_state("run").unwrap().unwrap().retry_count, 1);
        let budget = db.budget_snapshot("run").unwrap().unwrap();
        assert_eq!(
            budget.model_calls_used, 1,
            "budgets survive interruption and retry"
        );
        // A second concurrent retry cannot start another attempt.
        assert!(db.retry_interrupted_run("run", true).is_err());
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn gate_route_effect_is_claimed_once_and_ambiguity_is_never_auto_completed() {
        let (db, dir) = seed();
        let id = db
            .create_pending_effect(
                "run",
                "gate_route",
                "gate_route:gd1",
                &serde_json::json!({"decision": null}),
            )
            .unwrap();
        assert!(db.claim_pending_effect(&id).unwrap());
        assert!(!db.claim_pending_effect(&id).unwrap());
        assert!(db.complete_pending_effect(&id).unwrap());
        assert!(!db.complete_pending_effect(&id).unwrap());
        let ambiguous = db
            .create_pending_effect(
                "run",
                "publication",
                "publication:1",
                &serde_json::json!({}),
            )
            .unwrap();
        db.mark_pending_effect_ambiguous(&ambiguous, "unknown external effect")
            .unwrap();
        assert!(!db.complete_pending_effect(&ambiguous).unwrap());
        assert_eq!(db.list_pending_effects("run", true).unwrap().len(), 1);
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

pub(super) fn migrate(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS run_recovery (
            run_id TEXT PRIMARY KEY,
            interrupted_at TEXT NOT NULL,
            reason TEXT NOT NULL,
            resume_flow TEXT,
            resume_stage TEXT,
            effects_unknown INTEGER NOT NULL DEFAULT 0,
            retry_count INTEGER NOT NULL DEFAULT 0,
            updated_at TEXT NOT NULL
        );",
    )
    .context("Failed to create run_recovery table")?;
    Ok(())
}
