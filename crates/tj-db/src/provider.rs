//! Durably recorded provider attempts and token/cost usage (TJ-024).
//!
//! These rows are intentionally separate from the full model/tool transcript in
//! `stage_logs`. They answer "what happened at the transport boundary?" without
//! duplicating prompts, and are what diagnostics/support exports summarize.
//! Missing provider usage is stored as `usage_known = 0`, never as zero tokens.

use super::*;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderAttempt {
    pub id: i64,
    pub run_id: String,
    pub stage_id: String,
    pub stage_name: Option<String>,
    pub provider: String,
    pub model: String,
    pub outcome: String,
    pub error_class: Option<String>,
    pub http_status: Option<i64>,
    pub retry_index: u32,
    pub duration_ms: u64,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderUsage {
    pub run_id: String,
    pub operation_key: String,
    pub stage_id: String,
    pub model: String,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_micros: Option<u64>,
    pub usage_known: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderUsageSummary {
    pub calls: u64,
    pub known_usage_calls: u64,
    pub unknown_usage_calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_micros: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderAttemptSummary {
    pub total: u64,
    pub succeeded: u64,
    pub rate_limited: u64,
    pub transient: u64,
    pub credential: u64,
    pub permanent: u64,
}

pub(crate) fn migrate(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS provider_activity (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            run_id TEXT NOT NULL,
            stage_id TEXT NOT NULL,
            stage_name TEXT,
            provider TEXT NOT NULL,
            model TEXT NOT NULL,
            outcome TEXT NOT NULL,
            error_class TEXT,
            http_status INTEGER,
            retry_index INTEGER NOT NULL DEFAULT 0,
            duration_ms INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_provider_activity_run
            ON provider_activity(run_id, id);

        CREATE TABLE IF NOT EXISTS provider_usage (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            run_id TEXT NOT NULL,
            operation_key TEXT NOT NULL,
            stage_id TEXT NOT NULL,
            model TEXT NOT NULL,
            input_tokens INTEGER,
            output_tokens INTEGER,
            cost_micros INTEGER,
            usage_known INTEGER NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_provider_usage_operation
            ON provider_usage(run_id, operation_key);
        CREATE INDEX IF NOT EXISTS idx_provider_usage_run
            ON provider_usage(run_id, id);",
    )?;
    Ok(())
}

impl Database {
    #[allow(clippy::too_many_arguments)]
    pub fn record_provider_attempt(
        &self,
        run_id: &str,
        stage_id: &str,
        stage_name: Option<&str>,
        provider: &str,
        model: &str,
        outcome: &str,
        error_class: Option<&str>,
        http_status: Option<i64>,
        retry_index: u32,
        duration_ms: u64,
    ) -> Result<()> {
        let conn = lock_conn!(self);
        conn.execute(
            "INSERT INTO provider_activity
             (run_id, stage_id, stage_name, provider, model, outcome, error_class,
              http_status, retry_index, duration_ms, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                run_id,
                stage_id,
                stage_name,
                provider,
                model,
                outcome,
                error_class,
                http_status,
                retry_index as i64,
                duration_ms as i64,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        Ok(())
    }

    /// Record exactly-once usage for one model call. The operation key is the
    /// same durable budget key used for the call, so a transport retry cannot
    /// double-count tokens. Cost is updated only when token counts and pricing
    /// are both known; otherwise the call remains explicitly unknown.
    pub fn record_provider_usage(&self, usage: &ProviderUsage) -> Result<()> {
        anyhow::ensure!(
            !usage.operation_key.trim().is_empty(),
            "usage operation key is required"
        );
        let mut conn = lock_conn!(self);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let inserted = tx.execute(
            "INSERT INTO provider_usage
             (run_id, operation_key, stage_id, model, input_tokens, output_tokens,
              cost_micros, usage_known, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(run_id, operation_key) DO NOTHING",
            params![
                usage.run_id,
                usage.operation_key,
                usage.stage_id,
                usage.model,
                usage.input_tokens.map(|v| v as i64),
                usage.output_tokens.map(|v| v as i64),
                usage.cost_micros.map(|v| v as i64),
                usage.usage_known as i64,
                usage.created_at
            ],
        )?;
        if inserted == 1 {
            if usage.usage_known {
                if let Some(cost) = usage.cost_micros {
                    tx.execute(
                        "UPDATE run_budgets SET cost_micros_used = cost_micros_used + ?2,
                             updated_at = ?3 WHERE run_id = ?1",
                        params![usage.run_id, cost as i64, chrono::Utc::now().to_rfc3339()],
                    )?;
                }
            } else {
                tx.execute(
                    "UPDATE run_budgets SET unknown_usage_calls = unknown_usage_calls + 1,
                         updated_at = ?2 WHERE run_id = ?1",
                    params![usage.run_id, chrono::Utc::now().to_rfc3339()],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn list_provider_activity(
        &self,
        run_id: &str,
        after: i64,
        limit: i64,
    ) -> Result<Vec<ProviderAttempt>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(
            "SELECT id, run_id, stage_id, stage_name, provider, model, outcome,
                    error_class, http_status, retry_index, duration_ms, created_at
             FROM provider_activity WHERE run_id = ?1 AND id > ?2 ORDER BY id LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![run_id, after, limit], |row| {
            Ok(ProviderAttempt {
                id: row.get(0)?,
                run_id: row.get(1)?,
                stage_id: row.get(2)?,
                stage_name: row.get(3)?,
                provider: row.get(4)?,
                model: row.get(5)?,
                outcome: row.get(6)?,
                error_class: row.get(7)?,
                http_status: row.get(8)?,
                retry_index: row.get::<_, i64>(9)?.max(0) as u32,
                duration_ms: row.get::<_, i64>(10)?.max(0) as u64,
                created_at: row.get(11)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn provider_usage_summary(&self, run_id: &str) -> Result<ProviderUsageSummary> {
        let conn = lock_conn!(self);
        let summary = conn.query_row(
            "SELECT COUNT(1),
                    COALESCE(SUM(CASE WHEN usage_known = 1 THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN usage_known = 0 THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(input_tokens), 0),
                    COALESCE(SUM(output_tokens), 0),
                    COALESCE(SUM(cost_micros), 0)
             FROM provider_usage WHERE run_id = ?1",
            params![run_id],
            |row| {
                Ok(ProviderUsageSummary {
                    calls: row.get::<_, i64>(0)?.max(0) as u64,
                    known_usage_calls: row.get::<_, i64>(1)?.max(0) as u64,
                    unknown_usage_calls: row.get::<_, i64>(2)?.max(0) as u64,
                    input_tokens: row.get::<_, i64>(3)?.max(0) as u64,
                    output_tokens: row.get::<_, i64>(4)?.max(0) as u64,
                    cost_micros: row.get::<_, i64>(5)?.max(0) as u64,
                })
            },
        )?;
        Ok(summary)
    }

    pub fn provider_attempt_summary(&self, run_id: &str) -> Result<ProviderAttemptSummary> {
        let conn = lock_conn!(self);
        let mut summary = ProviderAttemptSummary::default();
        let mut stmt = conn.prepare(
            "SELECT outcome, error_class, COUNT(1) FROM provider_activity
             WHERE run_id = ?1 GROUP BY outcome, error_class",
        )?;
        let rows = stmt.query_map(params![run_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            let (outcome, class, count) = row?;
            let count = count.max(0) as u64;
            summary.total += count;
            if outcome == "success" {
                summary.succeeded += count;
            }
            match class.as_deref() {
                Some("rate_limit") => summary.rate_limited += count,
                Some("transient") => summary.transient += count,
                Some("credential") => summary.credential += count,
                Some(_) => summary.permanent += count,
                None => {}
            }
        }
        Ok(summary)
    }

    /// Bound durable activity retention. Run records, artifacts, decisions and
    /// packets are deliberately outside this scope; only replayable telemetry
    /// is pruned. Returns (stage_log_rows, provider_attempt_rows).
    pub fn prune_activity_before(&self, before: &str) -> Result<(usize, usize)> {
        let conn = lock_conn!(self);
        let logs = conn.execute(
            "DELETE FROM stage_logs WHERE created_at < ?1",
            params![before],
        )?;
        let attempts = conn.execute(
            "DELETE FROM provider_activity WHERE created_at < ?1",
            params![before],
        )?;
        Ok((logs, attempts))
    }

    pub fn latest_stage_log_id(&self, run_id: &str) -> Result<i64> {
        let conn = lock_conn!(self);
        Ok(conn
            .query_row(
                "SELECT COALESCE(MAX(id), 0) FROM stage_logs WHERE run_id = ?1",
                params![run_id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> (Database, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("tj-provider-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Database::open(&dir.join("state.db").to_string_lossy()).unwrap();
        db.init_run_budget(
            "run",
            &RunBudgetLimits {
                question_rounds_per_stage: 2,
                question_rounds_per_run: 3,
                model_calls: 5,
                tool_calls: 2,
                output_corrections: 2,
                interaction_nudges: 1,
                active_ms: 60_000,
                cost_micros: 100,
            },
        )
        .unwrap();
        (db, dir)
    }

    fn usage(key: &str, known: bool, cost: Option<u64>) -> ProviderUsage {
        ProviderUsage {
            run_id: "run".into(),
            operation_key: key.into(),
            stage_id: "stage".into(),
            model: "m".into(),
            input_tokens: known.then_some(10),
            output_tokens: known.then_some(5),
            cost_micros: cost,
            usage_known: known,
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    #[test]
    fn usage_is_idempotent_unknown_is_explicit_and_cost_cap_stops_new_calls() {
        let (db, _dir) = db();
        db.record_provider_usage(&usage("u1", true, Some(60)))
            .unwrap();
        db.record_provider_usage(&usage("u1", true, Some(60)))
            .unwrap();
        let snapshot = db.budget_snapshot("run").unwrap().unwrap();
        assert_eq!(snapshot.cost_micros_used, 60);
        assert_eq!(snapshot.unknown_usage_calls, 0);
        assert_eq!(db.provider_usage_summary("run").unwrap().calls, 1);

        db.record_provider_usage(&usage("u2", false, None)).unwrap();
        let snapshot = db.budget_snapshot("run").unwrap().unwrap();
        assert_eq!(snapshot.cost_micros_used, 60);
        assert_eq!(snapshot.unknown_usage_calls, 1);
        assert_eq!(
            db.provider_usage_summary("run")
                .unwrap()
                .unknown_usage_calls,
            1
        );

        db.record_provider_usage(&usage("u3", true, Some(50)))
            .unwrap();
        let error = db
            .charge_budget("run", BUDGET_MODEL_CALLS, Some("stage"), "m-next")
            .unwrap_err()
            .to_string();
        assert!(error.contains("cost_micros"), "{error}");
        assert!(
            db.budget_snapshot("run")
                .unwrap()
                .unwrap()
                .exhausted_reason
                .as_deref()
                .is_some_and(|reason| reason.starts_with("cost_micros"))
        );
    }

    #[test]
    fn attempt_summary_keeps_failure_classes_distinct() {
        let (db, _dir) = db();
        db.record_provider_attempt(
            "run",
            "stage",
            Some("intake"),
            "p",
            "m",
            "retryable",
            Some("transient"),
            Some(503),
            0,
            10,
        )
        .unwrap();
        db.record_provider_attempt(
            "run",
            "stage",
            Some("intake"),
            "p",
            "m",
            "retryable",
            Some("rate_limit"),
            Some(429),
            1,
            12,
        )
        .unwrap();
        db.record_provider_attempt(
            "run",
            "stage",
            Some("intake"),
            "p",
            "m",
            "failed",
            Some("credential"),
            Some(401),
            0,
            9,
        )
        .unwrap();
        db.record_provider_attempt(
            "run",
            "stage",
            Some("intake"),
            "p",
            "m",
            "success",
            None,
            None,
            2,
            20,
        )
        .unwrap();
        let summary = db.provider_attempt_summary("run").unwrap();
        assert_eq!(
            (
                summary.total,
                summary.transient,
                summary.rate_limited,
                summary.credential,
                summary.succeeded
            ),
            (4, 1, 1, 1, 1)
        );
        assert_eq!(db.list_provider_activity("run", 0, 10).unwrap().len(), 4);
    }
}
