//! Durable run budgets (TJ-016).
//!
//! Counters live on the run row and are only changed inside immediate SQLite
//! transactions. A charge carries an operation key with a unique index, so a
//! repeated call/retry with the same key is a replay rather than a second
//! charge; every genuinely new model/tool/question call is a new key and
//! consumes budget. Nothing is reset when a flow attempt, rework or explicit
//! retry starts.

use super::*;
use rusqlite::Transaction;

pub const BUDGET_QUESTION_ROUNDS: &str = "question_rounds";
pub const BUDGET_MODEL_CALLS: &str = "model_calls";
pub const BUDGET_TOOL_CALLS: &str = "tool_calls";
pub const BUDGET_OUTPUT_CORRECTIONS: &str = "output_corrections";
pub const BUDGET_INTERACTION_NUDGES: &str = "interaction_nudges";

/// Limits copied from the immutable execution bundle at admission.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunBudgetLimits {
    pub question_rounds_per_stage: u32,
    pub question_rounds_per_run: u32,
    pub model_calls: u32,
    pub tool_calls: u32,
    pub output_corrections: u32,
    pub interaction_nudges: u32,
    pub active_ms: u64,
    /// Monetary cap in micro-units; 0 disables it. Frozen from the execution
    /// bundle with the rest of the run limits.
    #[serde(default)]
    pub cost_micros: u64,
}

/// Observable state returned to the API/UI. Remaining values never go below
/// zero; `exhausted_reason` is the first durable denial, not a derived guess.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BudgetSnapshot {
    pub run_id: String,
    pub question_rounds_used: u32,
    pub question_rounds_limit: u32,
    pub question_rounds_remaining: u32,
    pub question_rounds_per_stage_limit: u32,
    pub model_calls_used: u32,
    pub model_calls_limit: u32,
    pub model_calls_remaining: u32,
    pub tool_calls_used: u32,
    pub tool_calls_limit: u32,
    pub tool_calls_remaining: u32,
    pub output_corrections_used: u32,
    pub output_corrections_limit: u32,
    pub output_corrections_remaining: u32,
    pub interaction_nudges_used: u32,
    pub interaction_nudges_limit: u32,
    pub interaction_nudges_remaining: u32,
    pub active_ms_used: u64,
    pub active_ms_limit: u64,
    pub active_ms_remaining: u64,
    /// Known provider cost in micro-units. Usage-less calls are counted in
    /// `unknown_usage_calls`, never treated as zero-cost.
    #[serde(default)]
    pub cost_micros_used: u64,
    #[serde(default)]
    pub cost_micros_limit: u64,
    #[serde(default)]
    pub cost_micros_remaining: u64,
    #[serde(default)]
    pub unknown_usage_calls: u64,
    pub exhausted: bool,
    pub exhausted_reason: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BudgetCharge {
    pub kind: String,
    pub scope: Option<String>,
    pub used: u32,
    pub limit: u32,
    pub remaining: u32,
    /// True when this charge was already recorded for the operation key.
    pub replayed: bool,
}

impl BudgetSnapshot {
    fn exhausted_reason(snapshot: &mut BudgetSnapshot) {
        let reasons = [
            (
                snapshot.question_rounds_used >= snapshot.question_rounds_limit,
                "question_rounds",
            ),
            (
                snapshot.model_calls_used >= snapshot.model_calls_limit,
                "model_calls",
            ),
            (
                snapshot.tool_calls_used >= snapshot.tool_calls_limit,
                "tool_calls",
            ),
            (
                snapshot.output_corrections_used >= snapshot.output_corrections_limit,
                "output_corrections",
            ),
            (
                snapshot.interaction_nudges_used >= snapshot.interaction_nudges_limit,
                "interaction_nudges",
            ),
            (
                snapshot.active_ms_used >= snapshot.active_ms_limit,
                "active_time",
            ),
            (
                snapshot.cost_micros_limit > 0
                    && snapshot.cost_micros_used >= snapshot.cost_micros_limit,
                "cost_micros",
            ),
        ];
        snapshot.exhausted =
            reasons.iter().any(|(hit, _)| *hit) || snapshot.exhausted_reason.is_some();
        if snapshot.exhausted_reason.is_none() {
            snapshot.exhausted_reason = reasons
                .iter()
                .find(|(hit, _)| *hit)
                .map(|(_, name)| (*name).to_string());
        }
    }
}

fn used(row: &rusqlite::Row<'_>, index: usize) -> std::result::Result<u32, rusqlite::Error> {
    let value: i64 = row.get(index)?;
    Ok(u32::try_from(value.max(0)).unwrap_or(u32::MAX))
}

fn snapshot_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<BudgetSnapshot> {
    let question_limit: i64 = row.get(2)?;
    let model_limit: i64 = row.get(5)?;
    let tool_limit: i64 = row.get(7)?;
    let correction_limit: i64 = row.get(9)?;
    let nudge_limit: i64 = row.get(11)?;
    let active_limit: i64 = row.get(13)?;
    let cost_used: i64 = row.get(16)?;
    let cost_limit: i64 = row.get(17)?;
    let mut snapshot = BudgetSnapshot {
        run_id: row.get(0)?,
        question_rounds_used: used(row, 1)?,
        question_rounds_limit: u32::try_from(question_limit.max(0)).unwrap_or(u32::MAX),
        question_rounds_remaining: 0,
        question_rounds_per_stage_limit: u32::try_from(row.get::<_, i64>(3)?.max(0))
            .unwrap_or(u32::MAX),
        model_calls_used: used(row, 4)?,
        model_calls_limit: u32::try_from(model_limit.max(0)).unwrap_or(u32::MAX),
        model_calls_remaining: 0,
        tool_calls_used: used(row, 6)?,
        tool_calls_limit: u32::try_from(tool_limit.max(0)).unwrap_or(u32::MAX),
        tool_calls_remaining: 0,
        output_corrections_used: used(row, 8)?,
        output_corrections_limit: u32::try_from(correction_limit.max(0)).unwrap_or(u32::MAX),
        output_corrections_remaining: 0,
        interaction_nudges_used: used(row, 10)?,
        interaction_nudges_limit: u32::try_from(nudge_limit.max(0)).unwrap_or(u32::MAX),
        interaction_nudges_remaining: 0,
        active_ms_used: u64::try_from(row.get::<_, i64>(12)?.max(0)).unwrap_or(u64::MAX),
        active_ms_limit: u64::try_from(active_limit.max(0)).unwrap_or(u64::MAX),
        active_ms_remaining: 0,
        cost_micros_used: u64::try_from(cost_used.max(0)).unwrap_or(u64::MAX),
        cost_micros_limit: u64::try_from(cost_limit.max(0)).unwrap_or(u64::MAX),
        cost_micros_remaining: 0,
        unknown_usage_calls: u64::try_from(row.get::<_, i64>(18)?.max(0)).unwrap_or(u64::MAX),
        exhausted: false,
        exhausted_reason: row.get(14)?,
        updated_at: row.get(15)?,
    };
    snapshot.question_rounds_remaining = snapshot
        .question_rounds_limit
        .saturating_sub(snapshot.question_rounds_used);
    snapshot.model_calls_remaining = snapshot
        .model_calls_limit
        .saturating_sub(snapshot.model_calls_used);
    snapshot.tool_calls_remaining = snapshot
        .tool_calls_limit
        .saturating_sub(snapshot.tool_calls_used);
    snapshot.output_corrections_remaining = snapshot
        .output_corrections_limit
        .saturating_sub(snapshot.output_corrections_used);
    snapshot.interaction_nudges_remaining = snapshot
        .interaction_nudges_limit
        .saturating_sub(snapshot.interaction_nudges_used);
    snapshot.active_ms_remaining = snapshot
        .active_ms_limit
        .saturating_sub(snapshot.active_ms_used);
    snapshot.cost_micros_remaining = if snapshot.cost_micros_limit == 0 {
        0
    } else {
        snapshot
            .cost_micros_limit
            .saturating_sub(snapshot.cost_micros_used)
    };
    BudgetSnapshot::exhausted_reason(&mut snapshot);
    Ok(snapshot)
}

const SELECT_BUDGET: &str = "SELECT run_id, question_rounds_used, question_rounds_limit,
    question_rounds_per_stage_limit, model_calls_used, model_calls_limit,
    tool_calls_used, tool_calls_limit, output_corrections_used, output_corrections_limit,
    interaction_nudges_used, interaction_nudges_limit, active_ms_used, active_ms_limit,
    exhausted_reason, updated_at, cost_micros_used, cost_micros_limit, unknown_usage_calls
    FROM run_budgets WHERE run_id = ?1";

impl Database {
    /// Create the immutable limits row once per run. Re-admission never
    /// replaces limits for an existing run.
    pub fn init_run_budget(&self, run_id: &str, limits: &RunBudgetLimits) -> Result<()> {
        anyhow::ensure!(!run_id.trim().is_empty(), "budget requires a run id");
        let conn = lock_conn!(self);
        conn.execute(
            "INSERT INTO run_budgets (run_id, question_rounds_used, question_rounds_limit,
                 question_rounds_per_stage_limit, model_calls_used, model_calls_limit,
                 tool_calls_used, tool_calls_limit, output_corrections_used, output_corrections_limit,
                 interaction_nudges_used, interaction_nudges_limit, active_ms_used, active_ms_limit,
                 exhausted_reason, updated_at, cost_micros_used, cost_micros_limit, unknown_usage_calls)
             VALUES (?1, 0, ?2, ?3, 0, ?4, 0, ?5, 0, ?6, 0, ?7, 0, ?8, NULL, ?9, 0, ?10, 0)
             ON CONFLICT(run_id) DO NOTHING",
            params![run_id, limits.question_rounds_per_run, limits.question_rounds_per_stage,
                limits.model_calls, limits.tool_calls, limits.output_corrections,
                limits.interaction_nudges, limits.active_ms,
                chrono::Utc::now().to_rfc3339(), limits.cost_micros],
        )?;
        Ok(())
    }

    pub fn has_run_budget(&self, run_id: &str) -> Result<bool> {
        let conn = lock_conn!(self);
        let exists: i64 = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM run_budgets WHERE run_id = ?1)",
            params![run_id],
            |row| row.get(0),
        )?;
        Ok(exists != 0)
    }

    pub fn budget_snapshot(&self, run_id: &str) -> Result<Option<BudgetSnapshot>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(SELECT_BUDGET)?;
        let mut rows = stmt.query(params![run_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(snapshot_from_row(row)?)),
            None => Ok(None),
        }
    }

    /// Charge one unit of `kind` for `operation_key`. Idempotent for the same
    /// key; a new operation key always consumes. Denial is durable: the
    /// exhausted reason is recorded once and returned in the error text.
    pub fn charge_budget(
        &self,
        run_id: &str,
        kind: &str,
        scope: Option<&str>,
        operation_key: &str,
    ) -> Result<BudgetCharge> {
        anyhow::ensure!(
            !operation_key.trim().is_empty(),
            "budget operation key is required"
        );
        let mut conn = lock_conn!(self);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let snapshot = {
            let mut stmt = tx.prepare(SELECT_BUDGET)?;
            let mut rows = stmt.query(params![run_id])?;
            match rows.next()? {
                Some(row) => snapshot_from_row(row)?,
                None => anyhow::bail!("run '{}' has no initialized budget", run_id),
            }
        };

        if let Some(existing) = tx.query_row(
            "SELECT amount FROM budget_charges WHERE run_id = ?1 AND kind = ?2 AND operation_key = ?3",
            params![run_id, kind, operation_key], |row| row.get::<_, i64>(0),
        ).optional()? {
            let _ = existing;
            let (used, limit) = budget_values(&snapshot, kind);
            return Ok(BudgetCharge {
                kind: kind.to_string(), scope: scope.map(str::to_string),
                used, limit, remaining: limit.saturating_sub(used), replayed: true,
            });
        }

        let (used, limit) = budget_values(&snapshot, kind);
        if used >= limit {
            deny_budget(&tx, run_id, kind, used, limit, scope)?;
            tx.commit()?;
            anyhow::bail!("budget exhausted: {} ({}/{})", kind, used, limit);
        }
        if kind == BUDGET_MODEL_CALLS
            && snapshot.cost_micros_limit > 0
            && snapshot.cost_micros_used >= snapshot.cost_micros_limit
        {
            deny_budget(
                &tx,
                run_id,
                "cost_micros",
                u32::try_from(snapshot.cost_micros_used).unwrap_or(u32::MAX),
                u32::try_from(snapshot.cost_micros_limit).unwrap_or(u32::MAX),
                scope,
            )?;
            tx.commit()?;
            anyhow::bail!(
                "budget exhausted: cost_micros ({}/{})",
                snapshot.cost_micros_used,
                snapshot.cost_micros_limit
            );
        }
        if kind == BUDGET_QUESTION_ROUNDS && scope.is_some() {
            let stage_used: i64 = tx.query_row(
                "SELECT COUNT(1) FROM budget_charges WHERE run_id = ?1 AND kind = ?2 AND scope = ?3",
                params![run_id, kind, scope], |row| row.get(0),
            )?;
            let stage_used = u32::try_from(stage_used.max(0)).unwrap_or(u32::MAX);
            if stage_used >= snapshot.question_rounds_per_stage_limit {
                deny_budget(
                    &tx,
                    run_id,
                    "question_rounds_per_stage",
                    stage_used,
                    snapshot.question_rounds_per_stage_limit,
                    scope,
                )?;
                tx.commit()?;
                anyhow::bail!(
                    "budget exhausted: question_rounds_per_stage ({}/{})",
                    stage_used,
                    snapshot.question_rounds_per_stage_limit
                );
            }
        }
        {
            let elapsed = open_interval_elapsed_ms(&tx, run_id, chrono::Utc::now())?;
            if snapshot.active_ms_used.saturating_add(elapsed) >= snapshot.active_ms_limit {
                deny_budget(
                    &tx,
                    run_id,
                    "active_time",
                    u32::try_from(snapshot.active_ms_used.saturating_add(elapsed))
                        .unwrap_or(u32::MAX),
                    u32::try_from(snapshot.active_ms_limit).unwrap_or(u32::MAX),
                    scope,
                )?;
                tx.commit()?;
                anyhow::bail!("budget exhausted: active_time");
            }
        }

        tx.execute(
            "INSERT INTO budget_charges (id, run_id, kind, scope, amount, operation_key, created_at)
             VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6)",
            params![format!("charge_{}", uuid::Uuid::new_v4()), run_id, kind, scope,
                operation_key, chrono::Utc::now().to_rfc3339()],
        )?;
        let column = match kind {
            BUDGET_QUESTION_ROUNDS => "question_rounds_used",
            BUDGET_MODEL_CALLS => "model_calls_used",
            BUDGET_TOOL_CALLS => "tool_calls_used",
            BUDGET_OUTPUT_CORRECTIONS => "output_corrections_used",
            BUDGET_INTERACTION_NUDGES => "interaction_nudges_used",
            other => anyhow::bail!("unknown budget kind '{}'", other),
        };
        tx.execute(
            &format!(
                "UPDATE run_budgets SET {column} = {column} + 1, updated_at = ?2 WHERE run_id = ?1"
            ),
            params![run_id, chrono::Utc::now().to_rfc3339()],
        )?;
        tx.commit()?;
        Ok(BudgetCharge {
            kind: kind.to_string(),
            scope: scope.map(str::to_string),
            used: used + 1,
            limit,
            remaining: limit.saturating_sub(used + 1),
            replayed: false,
        })
    }

    /// Open an active-time interval for a new execution attempt. Only one
    /// interval per run may be open; an attempted parallel execution fails
    /// closed instead of starting untracked work.
    pub fn start_budget_interval(&self, run_id: &str, attempt_key: &str) -> Result<()> {
        self.start_budget_interval_at(run_id, attempt_key, chrono::Utc::now())
    }

    pub fn start_budget_interval_at(
        &self,
        run_id: &str,
        attempt_key: &str,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<()> {
        let mut conn = lock_conn!(self);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let snapshot = {
            let mut stmt = tx.prepare(SELECT_BUDGET)?;
            let mut rows = stmt.query(params![run_id])?;
            match rows.next()? {
                Some(row) => snapshot_from_row(row)?,
                None => anyhow::bail!("run '{}' has no initialized budget", run_id),
            }
        };
        anyhow::ensure!(
            snapshot.active_ms_used < snapshot.active_ms_limit,
            "budget exhausted: active_time ({}/{})",
            snapshot.active_ms_used,
            snapshot.active_ms_limit
        );
        let open: i64 = tx.query_row(
            "SELECT COUNT(1) FROM budget_active_intervals WHERE run_id = ?1 AND ended_at IS NULL",
            params![run_id],
            |row| row.get(0),
        )?;
        anyhow::ensure!(
            open == 0,
            "run '{}' already has an active execution interval",
            run_id
        );
        tx.execute(
            "INSERT INTO budget_active_intervals (id, run_id, attempt_key, started_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                format!("interval_{}", uuid::Uuid::new_v4()),
                run_id,
                attempt_key,
                at.to_rfc3339()
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn finish_budget_interval(&self, run_id: &str, attempt_key: &str) -> Result<u64> {
        self.finish_budget_interval_at(
            run_id,
            attempt_key,
            chrono::Utc::now(),
            "execution finished",
        )
    }

    /// Close one attempt's open interval and add its whole elapsed time once.
    /// `finished_at` is injectable so tests and restart reconciliation use a
    /// bounded durable timestamp rather than unbounded wall-clock downtime.
    pub fn finish_budget_interval_at(
        &self,
        run_id: &str,
        attempt_key: &str,
        finished_at: chrono::DateTime<chrono::Utc>,
        reason: &str,
    ) -> Result<u64> {
        let mut conn = lock_conn!(self);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let elapsed = close_interval_tx(&tx, run_id, Some(attempt_key), finished_at, reason)?;
        tx.commit()?;
        Ok(elapsed)
    }

    /// Startup recovery: close any interval left open by a crashed process at
    /// a bounded timestamp (the run's last durable activity), never at "now"
    /// (which would count downtime as execution).
    pub fn reconcile_budget_intervals(
        &self,
        run_id: &str,
        ended_at: chrono::DateTime<chrono::Utc>,
        reason: &str,
    ) -> Result<u64> {
        let mut conn = lock_conn!(self);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let elapsed = close_interval_tx(&tx, run_id, None, ended_at, reason)?;
        tx.commit()?;
        Ok(elapsed)
    }

    pub fn stage_question_rounds_used(&self, run_id: &str, scope: &str) -> Result<u32> {
        let conn = lock_conn!(self);
        let n: i64 = conn.query_row(
            "SELECT COUNT(1) FROM budget_charges WHERE run_id = ?1 AND kind = 'question_rounds' AND scope = ?2",
            params![run_id, scope], |row| row.get(0),
        )?;
        Ok(u32::try_from(n.max(0)).unwrap_or(u32::MAX))
    }
}

fn budget_values(snapshot: &BudgetSnapshot, kind: &str) -> (u32, u32) {
    match kind {
        BUDGET_QUESTION_ROUNDS => (
            snapshot.question_rounds_used,
            snapshot.question_rounds_limit,
        ),
        BUDGET_MODEL_CALLS => (snapshot.model_calls_used, snapshot.model_calls_limit),
        BUDGET_TOOL_CALLS => (snapshot.tool_calls_used, snapshot.tool_calls_limit),
        BUDGET_OUTPUT_CORRECTIONS => (
            snapshot.output_corrections_used,
            snapshot.output_corrections_limit,
        ),
        BUDGET_INTERACTION_NUDGES => (
            snapshot.interaction_nudges_used,
            snapshot.interaction_nudges_limit,
        ),
        _ => (0, 0),
    }
}

fn deny_budget(
    tx: &Transaction<'_>,
    run_id: &str,
    kind: &str,
    used: u32,
    limit: u32,
    scope: Option<&str>,
) -> Result<()> {
    let reason = if kind == "question_rounds_per_stage" {
        format!("question_rounds_per_stage exhausted ({used}/{limit})")
    } else {
        format!("{kind} exhausted ({used}/{limit})")
    };
    tx.execute(
        "UPDATE run_budgets SET exhausted_reason = COALESCE(exhausted_reason, ?2), updated_at = ?3 WHERE run_id = ?1",
        params![run_id, reason, chrono::Utc::now().to_rfc3339()],
    )?;
    tx.execute(
        "INSERT INTO events (id, run_id, kind, stage_name, summary, payload, created_at)
         VALUES (?1, ?2, 'budget_exhausted', ?3, ?4, ?5, ?6)",
        params![
            format!("evt_{}", uuid::Uuid::new_v4()),
            run_id,
            scope,
            reason,
            serde_json::json!({"kind":kind,"used":used,"limit":limit,"scope":scope}).to_string(),
            chrono::Utc::now().to_rfc3339()
        ],
    )?;
    Ok(())
}

fn open_interval_elapsed_ms(
    tx: &Transaction<'_>,
    run_id: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<u64> {
    let started: Option<String> = tx.query_row(
        "SELECT started_at FROM budget_active_intervals WHERE run_id = ?1 AND ended_at IS NULL ORDER BY rowid DESC LIMIT 1",
        params![run_id], |row| row.get(0),
    ).optional()?;
    let Some(started) = started else {
        return Ok(0);
    };
    let started = chrono::DateTime::parse_from_rfc3339(&started)
        .map(|value| value.with_timezone(&chrono::Utc))
        .context("invalid budget interval timestamp")?;
    Ok(now.signed_duration_since(started).num_milliseconds().max(0) as u64)
}

fn close_interval_tx(
    tx: &Transaction<'_>,
    run_id: &str,
    attempt_key: Option<&str>,
    finished_at: chrono::DateTime<chrono::Utc>,
    reason: &str,
) -> Result<u64> {
    let row: Option<(String, String, String)> = if let Some(attempt) = attempt_key {
        tx.query_row(
            "SELECT id, attempt_key, started_at FROM budget_active_intervals
             WHERE run_id = ?1 AND attempt_key = ?2 AND ended_at IS NULL",
            params![run_id, attempt],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
    } else {
        tx.query_row(
            "SELECT id, attempt_key, started_at FROM budget_active_intervals
             WHERE run_id = ?1 AND ended_at IS NULL ORDER BY rowid DESC LIMIT 1",
            params![run_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
    };
    let Some((id, attempt, started)) = row else {
        return Ok(0);
    };
    let started = chrono::DateTime::parse_from_rfc3339(&started)
        .map(|value| value.with_timezone(&chrono::Utc))
        .context("invalid budget interval timestamp")?;
    let elapsed = if finished_at > started {
        finished_at
            .signed_duration_since(started)
            .num_milliseconds()
            .max(0) as u64
    } else {
        0
    };
    tx.execute(
        "UPDATE budget_active_intervals SET ended_at = ?2, close_reason = ?3 WHERE id = ?1",
        params![id, finished_at.to_rfc3339(), reason],
    )?;
    tx.execute(
        "UPDATE run_budgets SET active_ms_used = active_ms_used + ?2, updated_at = ?3 WHERE run_id = ?1",
        params![run_id, elapsed as i64, chrono::Utc::now().to_rfc3339()],
    )?;
    let snapshot = {
        let mut stmt = tx.prepare(SELECT_BUDGET)?;
        let mut rows = stmt.query(params![run_id])?;
        let Some(row) = rows.next()? else {
            return Ok(elapsed);
        };
        snapshot_from_row(row)?
    };
    if snapshot.active_ms_used >= snapshot.active_ms_limit {
        deny_budget(
            tx,
            run_id,
            "active_time",
            u32::try_from(snapshot.active_ms_used).unwrap_or(u32::MAX),
            u32::try_from(snapshot.active_ms_limit).unwrap_or(u32::MAX),
            Some(&attempt),
        )?;
    }
    Ok(elapsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn test_db() -> (Arc<Database>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("tj-budget-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Database::open(&dir.join("state.db").to_string_lossy()).unwrap());
        (db, dir)
    }

    fn limits() -> RunBudgetLimits {
        RunBudgetLimits {
            question_rounds_per_stage: 2,
            question_rounds_per_run: 3,
            model_calls: 4,
            tool_calls: 3,
            output_corrections: 2,
            interaction_nudges: 1,
            active_ms: 10_000,
            cost_micros: 0,
        }
    }

    #[test]
    fn charges_are_persistent_idempotent_and_explicitly_exhausted() {
        let (db, dir) = test_db();
        db.init_run_budget("run", &limits()).unwrap();
        let first = db
            .charge_budget("run", BUDGET_MODEL_CALLS, Some("stage"), "m1")
            .unwrap();
        assert_eq!((first.used, first.remaining, first.replayed), (1, 3, false));
        let replay = db
            .charge_budget("run", BUDGET_MODEL_CALLS, Some("stage"), "m1")
            .unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.used, 1);
        for key in ["m2", "m3", "m4"] {
            db.charge_budget("run", BUDGET_MODEL_CALLS, Some("stage"), key)
                .unwrap();
        }
        let error = db
            .charge_budget("run", BUDGET_MODEL_CALLS, Some("stage"), "m5")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("budget exhausted: model_calls (4/4)"),
            "{error}"
        );
        let snapshot = db.budget_snapshot("run").unwrap().unwrap();
        assert!(snapshot.exhausted);
        assert_eq!(
            snapshot.exhausted_reason.as_deref(),
            Some("model_calls exhausted (4/4)")
        );
        // A new attempt/flow does not reset counters.
        assert_eq!(
            db.budget_snapshot("run").unwrap().unwrap().model_calls_used,
            4
        );
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn question_budget_is_separate_per_stage_and_run() {
        let (db, dir) = test_db();
        db.init_run_budget("run", &limits()).unwrap();
        for key in ["q1", "q2"] {
            db.charge_budget("run", BUDGET_QUESTION_ROUNDS, Some("stage-a"), key)
                .unwrap();
        }
        let error = db
            .charge_budget("run", BUDGET_QUESTION_ROUNDS, Some("stage-a"), "q3")
            .unwrap_err()
            .to_string();
        assert!(error.contains("question_rounds_per_stage"), "{error}");
        db.charge_budget("run", BUDGET_QUESTION_ROUNDS, Some("stage-b"), "q4")
            .unwrap();
        let error = db
            .charge_budget("run", BUDGET_QUESTION_ROUNDS, Some("stage-b"), "q5")
            .unwrap_err()
            .to_string();
        assert!(error.contains("question_rounds (3/3)"), "{error}");
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn parallel_charges_cannot_exceed_the_bound() {
        let (db, dir) = test_db();
        db.init_run_budget("run", &limits()).unwrap();
        let mut handles = Vec::new();
        for index in 0..20 {
            let db = Arc::clone(&db);
            handles.push(std::thread::spawn(move || {
                db.charge_budget("run", BUDGET_MODEL_CALLS, Some("s"), &format!("k{index}"))
                    .is_ok()
            }));
        }
        let accepted = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .filter(|accepted| *accepted)
            .count();
        assert_eq!(accepted, 4);
        assert_eq!(
            db.budget_snapshot("run").unwrap().unwrap().model_calls_used,
            4
        );
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn active_time_uses_injected_clock_and_survives_attempts() {
        let (db, dir) = test_db();
        db.init_run_budget("run", &limits()).unwrap();
        let start = chrono::DateTime::parse_from_rfc3339("2026-10-22T10:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        db.start_budget_interval_at("run", "attempt-1", start)
            .unwrap();
        let elapsed = db
            .finish_budget_interval_at(
                "run",
                "attempt-1",
                start + chrono::Duration::milliseconds(4000),
                "flow done",
            )
            .unwrap();
        assert_eq!(elapsed, 4000);
        let snapshot = db.budget_snapshot("run").unwrap().unwrap();
        assert_eq!(snapshot.active_ms_used, 4000);
        // A second attempt adds, never resets.
        db.start_budget_interval_at(
            "run",
            "attempt-2",
            start + chrono::Duration::milliseconds(5000),
        )
        .unwrap();
        db.finish_budget_interval_at(
            "run",
            "attempt-2",
            start + chrono::Duration::milliseconds(11000),
            "flow done",
        )
        .unwrap();
        let snapshot = db.budget_snapshot("run").unwrap().unwrap();
        assert_eq!(snapshot.active_ms_used, 10000);
        assert!(snapshot.active_ms_remaining == 0);
        let error = db
            .start_budget_interval("run", "attempt-3")
            .unwrap_err()
            .to_string();
        assert!(error.contains("budget exhausted: active_time"), "{error}");
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

pub(super) fn migrate(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS run_budgets (
            run_id TEXT PRIMARY KEY,
            question_rounds_used INTEGER NOT NULL DEFAULT 0,
            question_rounds_limit INTEGER NOT NULL,
            question_rounds_per_stage_limit INTEGER NOT NULL,
            model_calls_used INTEGER NOT NULL DEFAULT 0,
            model_calls_limit INTEGER NOT NULL,
            tool_calls_used INTEGER NOT NULL DEFAULT 0,
            tool_calls_limit INTEGER NOT NULL,
            output_corrections_used INTEGER NOT NULL DEFAULT 0,
            output_corrections_limit INTEGER NOT NULL,
            interaction_nudges_used INTEGER NOT NULL DEFAULT 0,
            interaction_nudges_limit INTEGER NOT NULL,
            active_ms_used INTEGER NOT NULL DEFAULT 0,
            active_ms_limit INTEGER NOT NULL,
            exhausted_reason TEXT,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS budget_charges (
            id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            scope TEXT,
            amount INTEGER NOT NULL DEFAULT 1,
            operation_key TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_budget_charge_key
            ON budget_charges(run_id, kind, operation_key);
        CREATE INDEX IF NOT EXISTS idx_budget_charge_scope
            ON budget_charges(run_id, kind, scope);
        CREATE TABLE IF NOT EXISTS budget_active_intervals (
            id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL,
            attempt_key TEXT NOT NULL,
            started_at TEXT NOT NULL,
            ended_at TEXT,
            close_reason TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_budget_interval_run
            ON budget_active_intervals(run_id, ended_at);",
    )
    .context("Failed to create run budget tables")?;
    for (column, declaration) in [
        ("cost_micros_used", "INTEGER NOT NULL DEFAULT 0"),
        ("cost_micros_limit", "INTEGER NOT NULL DEFAULT 0"),
        ("unknown_usage_calls", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        if conn
            .prepare(&format!("SELECT {column} FROM run_budgets LIMIT 0"))
            .is_err()
        {
            conn.execute_batch(&format!(
                "ALTER TABLE run_budgets ADD COLUMN {column} {declaration};"
            ))?;
        }
    }
    Ok(())
}
