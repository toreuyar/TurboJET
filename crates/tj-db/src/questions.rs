//! Durable, question-addressed input (TJ-014).
//!
//! A question is the unit of acceptance, not a run or a stage. The row binds
//! the run, flow, stage execution id, stage name and attempt number. Exactly
//! one transport (webui, Zulip, API) can move `open -> answered`; retries with
//! the same idempotency key replay the accepted answer, while late, duplicate
//! or cross-stage replies conflict visibly. Nothing here stores credentials.

use super::*;

/// At most this many questions may be open for one run. A model that asks in
/// a tight loop is bounded rather than growing the durable queue forever.
pub const MAX_PENDING_QUESTIONS_PER_RUN: i64 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionStatus {
    Open,
    Answered,
    Closed,
    Cancelled,
}

impl QuestionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            QuestionStatus::Open => "open",
            QuestionStatus::Answered => "answered",
            QuestionStatus::Closed => "closed",
            QuestionStatus::Cancelled => "cancelled",
        }
    }
}

impl FromStr for QuestionStatus {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "open" => Ok(QuestionStatus::Open),
            "answered" => Ok(QuestionStatus::Answered),
            "closed" => Ok(QuestionStatus::Closed),
            "cancelled" => Ok(QuestionStatus::Cancelled),
            other => Err(format!("Unknown QuestionStatus: {other}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionRecord {
    pub id: String,
    pub run_id: RunId,
    pub flow_id: String,
    pub stage_id: String,
    pub stage_name: String,
    pub flow_name: String,
    pub attempt: u32,
    /// Canonical `QuestionRequest` JSON.
    pub request: String,
    pub status: QuestionStatus,
    /// Canonical `QuestionAnswer` JSON once accepted.
    pub answer: Option<String>,
    pub answer_source: Option<String>,
    pub answer_idempotency_key: Option<String>,
    pub created_at: String,
    pub answered_at: Option<String>,
    pub closed_at: Option<String>,
    pub close_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionAcceptance {
    pub question: QuestionRecord,
    /// True when the same idempotency key/answer was already accepted.
    pub replayed: bool,
}

impl QuestionRecord {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: String,
        run_id: RunId,
        flow_id: String,
        stage_id: String,
        stage_name: String,
        flow_name: String,
        attempt: u32,
        request: String,
    ) -> Self {
        Self {
            id,
            run_id,
            flow_id,
            stage_id,
            stage_name,
            flow_name,
            attempt,
            request,
            status: QuestionStatus::Open,
            answer: None,
            answer_source: None,
            answer_idempotency_key: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            answered_at: None,
            closed_at: None,
            close_reason: None,
        }
    }
}

fn read_question_row(row: &rusqlite::Row) -> rusqlite::Result<QuestionRecord> {
    let status: String = row.get(8)?;
    Ok(QuestionRecord {
        id: row.get(0)?,
        run_id: RunId(row.get(1)?),
        flow_id: row.get(2)?,
        stage_id: row.get(3)?,
        stage_name: row.get(4)?,
        flow_name: row.get(5)?,
        attempt: row.get(6)?,
        request: row.get(7)?,
        status: status.parse().unwrap_or(QuestionStatus::Closed),
        answer: row.get(9)?,
        answer_source: row.get(10)?,
        answer_idempotency_key: row.get(11)?,
        created_at: row.get(12)?,
        answered_at: row.get(13)?,
        closed_at: row.get(14)?,
        close_reason: row.get(15)?,
    })
}

const QUESTION_COLUMNS: &str = "id, run_id, flow_id, stage_id, stage_name, flow_name, attempt, request, status, answer, answer_source, answer_idempotency_key, created_at, answered_at, closed_at, close_reason";
const QUESTION_SELECT: &str = "SELECT id, run_id, flow_id, stage_id, stage_name, flow_name, attempt, request, status, answer, answer_source, answer_idempotency_key, created_at, answered_at, closed_at, close_reason FROM questions";

impl Database {
    /// Persist a new open question. Any older open question for the same stage
    /// execution is cancelled first (superseded), and the per-run pending
    /// bound is enforced in the same transaction.
    pub fn create_question(&self, question: &QuestionRecord) -> Result<()> {
        anyhow::ensure!(
            question.status == QuestionStatus::Open,
            "new question must be open"
        );
        let mut conn = lock_conn!(self);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let now = chrono::Utc::now().to_rfc3339();
        tx.execute(
            "UPDATE questions SET status = 'cancelled', closed_at = ?1,
                 close_reason = 'superseded by a newer question'
             WHERE stage_id = ?2 AND status = 'open'",
            params![now, question.stage_id],
        )?;
        let open_count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM questions WHERE run_id = ?1 AND status = 'open'",
            params![question.run_id.as_str()],
            |row| row.get(0),
        )?;
        anyhow::ensure!(
            open_count < MAX_PENDING_QUESTIONS_PER_RUN,
            "run '{}' already has {} pending questions (bound {})",
            question.run_id.as_str(),
            open_count,
            MAX_PENDING_QUESTIONS_PER_RUN
        );
        tx.execute(
            &format!(
                "INSERT INTO questions ({QUESTION_COLUMNS})
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)"
            ),
            params![
                question.id,
                question.run_id.as_str(),
                question.flow_id,
                question.stage_id,
                question.stage_name,
                question.flow_name,
                question.attempt,
                question.request,
                question.status.as_str(),
                question.answer,
                question.answer_source,
                question.answer_idempotency_key,
                question.created_at,
                question.answered_at,
                question.closed_at,
                question.close_reason,
            ],
        )
        .with_context(|| format!("Failed to persist question {}", question.id))?;
        tx.commit()?;
        Ok(())
    }

    pub fn load_question(&self, id: &str) -> Result<Option<QuestionRecord>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(&format!("{QUESTION_SELECT} WHERE id = ?1"))?;
        let mut rows = stmt.query(params![id])?;
        match rows.next()? {
            Some(row) => Ok(Some(read_question_row(row)?)),
            None => Ok(None),
        }
    }

    pub fn list_questions_for_run(
        &self,
        run_id: &str,
        open_only: bool,
    ) -> Result<Vec<QuestionRecord>> {
        let conn = lock_conn!(self);
        let sql = if open_only {
            format!("{QUESTION_SELECT} WHERE run_id = ?1 AND status = 'open' ORDER BY rowid")
        } else {
            format!("{QUESTION_SELECT} WHERE run_id = ?1 ORDER BY rowid")
        };
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![run_id], read_question_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn open_question_for_stage(&self, stage_id: &str) -> Result<Option<QuestionRecord>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(&format!(
            "{QUESTION_SELECT} WHERE stage_id = ?1 AND status = 'open' ORDER BY rowid DESC LIMIT 1"
        ))?;
        let mut rows = stmt.query(params![stage_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(read_question_row(row)?)),
            None => Ok(None),
        }
    }

    pub fn run_has_open_question(&self, run_id: &str) -> Result<bool> {
        let conn = lock_conn!(self);
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM questions WHERE run_id = ?1 AND status = 'open'",
            params![run_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Atomically accept one answer for the intended open question.
    ///
    /// Identity is checked against the run, stage execution id and attempt when
    /// supplied by the caller. The stage must still be waiting for input and
    /// the run must still be running. A duplicate with the same idempotency key
    /// and identical answer replays; every other duplicate/late/cross-stage
    /// reply is a conflict.
    pub fn answer_question(
        &self,
        question_id: &str,
        run_id: &str,
        stage_id: &str,
        attempt: Option<u32>,
        answer: &str,
        source: &str,
        idempotency_key: &str,
    ) -> Result<QuestionAcceptance> {
        anyhow::ensure!(
            !idempotency_key.trim().is_empty(),
            "an idempotency key is required"
        );
        anyhow::ensure!(!answer.trim().is_empty(), "an answer is required");
        anyhow::ensure!(!source.trim().is_empty(), "an answer source is required");

        let mut conn = lock_conn!(self);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut question = {
            let mut stmt = tx.prepare(&format!("{QUESTION_SELECT} WHERE id = ?1"))?;
            let mut rows = stmt.query(params![question_id])?;
            match rows.next()? {
                Some(row) => read_question_row(row)?,
                None => anyhow::bail!("question '{}' does not exist", question_id),
            }
        };

        anyhow::ensure!(
            question.run_id.as_str() == run_id,
            "question '{}' belongs to run '{}', not '{}'",
            question_id,
            question.run_id.as_str(),
            run_id
        );
        anyhow::ensure!(
            question.stage_id == stage_id,
            "cross-stage reply rejected: question '{}' belongs to stage '{}', not '{}'",
            question_id,
            question.stage_id,
            stage_id
        );
        if let Some(expected_attempt) = attempt {
            anyhow::ensure!(
                question.attempt == expected_attempt,
                "stale attempt reply rejected: question '{}' is bound to attempt {}, not {}",
                question_id,
                question.attempt,
                expected_attempt
            );
        }

        if question.status == QuestionStatus::Answered {
            let same_key = question.answer_idempotency_key.as_deref() == Some(idempotency_key);
            let same_answer = question.answer.as_deref() == Some(answer);
            anyhow::ensure!(
                same_key && same_answer,
                "question '{}' was already answered by {}; this reply is late or conflicting",
                question_id,
                question
                    .answer_source
                    .as_deref()
                    .unwrap_or("another transport")
            );
            return Ok(QuestionAcceptance {
                question,
                replayed: true,
            });
        }
        anyhow::ensure!(
            question.status == QuestionStatus::Open,
            "question '{}' is {}; a {} reply cannot be accepted",
            question_id,
            question.status.as_str(),
            question.status.as_str()
        );

        let stage_status: String = tx
            .query_row(
                "SELECT status FROM stages WHERE id = ?1",
                params![stage_id],
                |row| row.get(0),
            )
            .with_context(|| format!("answered stage '{}' does not exist", stage_id))?;
        anyhow::ensure!(
            stage_status == "waiting_for_input",
            "stage '{}' is not waiting for input (status '{}'); refusing a stale reply",
            question.stage_name,
            stage_status
        );
        let run_status: String = tx
            .query_row(
                "SELECT status FROM pipeline_runs WHERE id = ?1",
                params![run_id],
                |row| row.get(0),
            )
            .with_context(|| format!("answered run '{}' does not exist", run_id))?;
        anyhow::ensure!(
            run_status == "running",
            "run '{}' is {}; question '{}' is closed",
            run_id,
            run_status,
            question_id
        );

        let now = chrono::Utc::now().to_rfc3339();
        let changed = tx.execute(
            "UPDATE questions SET status = 'answered', answer = ?1, answer_source = ?2,
                 answer_idempotency_key = ?3, answered_at = ?4
             WHERE id = ?5 AND status = 'open'",
            params![answer, source, idempotency_key, now, question_id],
        )?;
        if changed != 1 {
            anyhow::bail!(
                "question '{}' is no longer open; another reply won the race",
                question_id
            );
        }
        tx.execute(
            "INSERT INTO events (id, run_id, kind, actor, flow_name, stage_name, payload, created_at)
             VALUES (?1, ?2, 'question_answered', ?3, ?4, ?5, ?6, ?7)",
            params![
                format!("evt_{}", uuid::Uuid::new_v4()),
                run_id,
                source,
                question.flow_name,
                question.stage_name,
                serde_json::json!({
                    "question_id": question_id,
                    "stage_id": stage_id,
                    "attempt": question.attempt,
                    "source": source,
                }).to_string(),
                now
            ],
        )?;
        question.status = QuestionStatus::Answered;
        question.answer = Some(answer.to_string());
        question.answer_source = Some(source.to_string());
        question.answer_idempotency_key = Some(idempotency_key.to_string());
        question.answered_at = Some(now);
        tx.commit()?;
        Ok(QuestionAcceptance {
            question,
            replayed: false,
        })
    }

    /// Close/cancel an open question. Conditional on `open`, so a late caller
    /// can never rewrite an accepted answer.
    pub fn close_question(
        &self,
        question_id: &str,
        status: QuestionStatus,
        reason: &str,
    ) -> Result<bool> {
        anyhow::ensure!(
            matches!(status, QuestionStatus::Closed | QuestionStatus::Cancelled),
            "question can only be closed or cancelled"
        );
        let conn = lock_conn!(self);
        let changed = conn.execute(
            "UPDATE questions SET status = ?1, closed_at = ?2, close_reason = ?3
             WHERE id = ?4 AND status = 'open'",
            params![
                status.as_str(),
                chrono::Utc::now().to_rfc3339(),
                reason,
                question_id
            ],
        )?;
        Ok(changed == 1)
    }

    /// Close every open question for a run (terminal run / recovery).
    pub fn cancel_open_questions_for_run(&self, run_id: &str, reason: &str) -> Result<u64> {
        let conn = lock_conn!(self);
        let changed = conn.execute(
            "UPDATE questions SET status = 'cancelled', closed_at = ?1, close_reason = ?2
             WHERE run_id = ?3 AND status = 'open'",
            params![chrono::Utc::now().to_rfc3339(), reason, run_id],
        )?;
        Ok(changed as u64)
    }

    /// Close open questions belonging to one stage execution.
    pub fn cancel_open_questions_for_stage(&self, stage_id: &str, reason: &str) -> Result<u64> {
        let conn = lock_conn!(self);
        let changed = conn.execute(
            "UPDATE questions SET status = 'cancelled', closed_at = ?1, close_reason = ?2
             WHERE stage_id = ?3 AND status = 'open'",
            params![chrono::Utc::now().to_rfc3339(), reason, stage_id],
        )?;
        Ok(changed as u64)
    }
}

/// Read an optional question row inside an existing transaction (used by
/// migration/finalization paths that cannot call `Database` methods).
pub(super) fn close_open_for_run_tx(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
    reason: &str,
) -> Result<usize> {
    let changed = tx.execute(
        "UPDATE questions SET status = 'cancelled', closed_at = ?1, close_reason = ?2
         WHERE run_id = ?3 AND status = 'open'",
        params![chrono::Utc::now().to_rfc3339(), reason, run_id],
    )?;
    Ok(changed)
}
