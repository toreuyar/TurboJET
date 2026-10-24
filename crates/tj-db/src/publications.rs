//! Durable remote-publication identity and state (TJ-036).
//!
//! A publication row is written *before* any push or PR call. Its operation
//! key, commit SHA, branch and exact task/plan/packet/artifact identities are
//! immutable. State transitions are monotonic in the normal path, and a
//! `running`/`unknown` row must be reconciled against the remote before a
//! retry. A failed push can never be recorded as published success.

use super::*;
use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationState {
    Pending,
    Running,
    Succeeded,
    Failed,
    Unknown,
}

impl PublicationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "unknown" => Ok(Self::Unknown),
            other => anyhow::bail!("unknown publication state '{other}'"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewPublication {
    pub id: String,
    pub run_id: String,
    pub flow_name: String,
    pub attempt_id: String,
    pub operation_key: String,
    pub selection_id: String,
    pub task_id: String,
    pub plan_revision_id: String,
    pub plan_content_hash: String,
    pub packet_revision_id: String,
    pub packet_content_hash: String,
    pub branch: String,
    pub base_branch: String,
    pub workspace_path: String,
    pub base_sha: Option<String>,
    pub commit_sha: Option<String>,
    pub test_artifact_id: String,
    pub test_artifact_hash: String,
    pub review_artifact_id: String,
    pub review_artifact_hash: String,
    pub acceptance_artifact_id: String,
    pub acceptance_artifact_hash: String,
    pub scope_files_json: String,
    pub scope_violations_json: String,
    /// Parameterized declared evidence (role, stage, artifact identity). `None`
    /// is the additive legacy shape and readers fall back to the named columns.
    pub evidence_json: Option<String>,
    pub draft: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemotePublication {
    pub id: String,
    pub run_id: String,
    pub flow_name: String,
    pub attempt_id: String,
    pub operation_key: String,
    pub selection_id: String,
    pub task_id: String,
    pub plan_revision_id: String,
    pub plan_content_hash: String,
    pub packet_revision_id: String,
    pub packet_content_hash: String,
    pub branch: String,
    pub base_branch: String,
    pub workspace_path: String,
    pub base_sha: Option<String>,
    pub commit_sha: Option<String>,
    pub test_artifact_id: String,
    pub test_artifact_hash: String,
    pub review_artifact_id: String,
    pub review_artifact_hash: String,
    pub acceptance_artifact_id: String,
    pub acceptance_artifact_hash: String,
    pub scope_files_json: String,
    pub scope_violations_json: String,
    pub evidence_json: Option<String>,
    pub push_state: PublicationState,
    pub push_error: Option<String>,
    pub remote_sha: Option<String>,
    pub pr_state: PublicationState,
    pub pr_number: Option<u64>,
    pub pr_url: Option<String>,
    pub pr_error: Option<String>,
    pub draft: bool,
    pub created_at: String,
    pub updated_at: String,
}

const COLUMNS: &str = "id, run_id, flow_name, attempt_id, operation_key, selection_id, task_id,
     plan_revision_id, plan_content_hash, packet_revision_id, packet_content_hash,
     branch, base_branch, workspace_path, base_sha, commit_sha,
     test_artifact_id, test_artifact_hash, review_artifact_id, review_artifact_hash,
     acceptance_artifact_id, acceptance_artifact_hash, scope_files_json,
     scope_violations_json, evidence_json, push_state, push_error, remote_sha,
     pr_state, pr_number, pr_url, pr_error, draft, created_at, updated_at";

fn read_publication(row: &rusqlite::Row<'_>) -> rusqlite::Result<RemotePublication> {
    let push_state: String = row.get(25)?;
    let pr_state: String = row.get(28)?;
    Ok(RemotePublication {
        id: row.get(0)?,
        run_id: row.get(1)?,
        flow_name: row.get(2)?,
        attempt_id: row.get(3)?,
        operation_key: row.get(4)?,
        selection_id: row.get(5)?,
        task_id: row.get(6)?,
        plan_revision_id: row.get(7)?,
        plan_content_hash: row.get(8)?,
        packet_revision_id: row.get(9)?,
        packet_content_hash: row.get(10)?,
        branch: row.get(11)?,
        base_branch: row.get(12)?,
        workspace_path: row.get(13)?,
        base_sha: row.get(14)?,
        commit_sha: row.get(15)?,
        test_artifact_id: row.get(16)?,
        test_artifact_hash: row.get(17)?,
        review_artifact_id: row.get(18)?,
        review_artifact_hash: row.get(19)?,
        acceptance_artifact_id: row.get(20)?,
        acceptance_artifact_hash: row.get(21)?,
        scope_files_json: row.get(22)?,
        scope_violations_json: row.get(23)?,
        evidence_json: row.get(24)?,
        push_state: PublicationState::parse(&push_state)
            .map_err(|error| rusqlite::Error::InvalidParameterName(error.to_string()))?,
        push_error: row.get(26)?,
        remote_sha: row.get(27)?,
        pr_state: PublicationState::parse(&pr_state)
            .map_err(|error| rusqlite::Error::InvalidParameterName(error.to_string()))?,
        pr_number: row.get(29)?,
        pr_url: row.get(30)?,
        pr_error: row.get(31)?,
        draft: row.get::<_, i64>(32)? != 0,
        created_at: row.get(33)?,
        updated_at: row.get(34)?,
    })
}

pub(crate) fn migrate(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS remote_publications (
            id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL,
            flow_name TEXT NOT NULL,
            attempt_id TEXT NOT NULL,
            operation_key TEXT NOT NULL,
            selection_id TEXT NOT NULL,
            task_id TEXT NOT NULL,
            plan_revision_id TEXT NOT NULL,
            plan_content_hash TEXT NOT NULL,
            packet_revision_id TEXT NOT NULL,
            packet_content_hash TEXT NOT NULL,
            branch TEXT NOT NULL,
            base_branch TEXT NOT NULL,
            workspace_path TEXT NOT NULL,
            base_sha TEXT,
            commit_sha TEXT,
            test_artifact_id TEXT NOT NULL,
            test_artifact_hash TEXT NOT NULL,
            review_artifact_id TEXT NOT NULL,
            review_artifact_hash TEXT NOT NULL,
            acceptance_artifact_id TEXT NOT NULL,
            acceptance_artifact_hash TEXT NOT NULL,
            scope_files_json TEXT NOT NULL,
            scope_violations_json TEXT NOT NULL,
            push_state TEXT NOT NULL,
            push_error TEXT,
            remote_sha TEXT,
            pr_state TEXT NOT NULL,
            pr_number INTEGER,
            pr_url TEXT,
            pr_error TEXT,
            draft INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_remote_publications_operation
            ON remote_publications(operation_key);
        CREATE INDEX IF NOT EXISTS idx_remote_publications_run
            ON remote_publications(run_id, created_at);
        CREATE INDEX IF NOT EXISTS idx_remote_publications_branch
            ON remote_publications(branch);",
    )?;
    if conn
        .prepare("SELECT evidence_json FROM remote_publications LIMIT 0")
        .is_err()
    {
        conn.execute_batch("ALTER TABLE remote_publications ADD COLUMN evidence_json TEXT;")?;
    }
    Ok(())
}

impl Database {
    /// Idempotent insert. A duplicate operation key returns the existing row and
    /// `false` (not replayed) so callers never create a second remote operation.
    pub fn create_publication(
        &self,
        publication: &NewPublication,
    ) -> Result<(RemotePublication, bool)> {
        let mut conn = lock_conn!(self);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let Some(existing) = tx
            .query_row(
                &format!("SELECT {COLUMNS} FROM remote_publications WHERE operation_key = ?1"),
                params![publication.operation_key],
                read_publication,
            )
            .optional()?
        {
            return Ok((existing, false));
        }
        tx.execute(
            "INSERT INTO remote_publications
             (id, run_id, flow_name, attempt_id, operation_key, selection_id, task_id,
              plan_revision_id, plan_content_hash, packet_revision_id, packet_content_hash,
              branch, base_branch, workspace_path, base_sha, commit_sha,
              test_artifact_id, test_artifact_hash, review_artifact_id, review_artifact_hash,
              acceptance_artifact_id, acceptance_artifact_hash, scope_files_json,
              scope_violations_json, evidence_json, push_state, push_error, remote_sha,
              pr_state, pr_number, pr_url, pr_error, draft, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                     ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25,
                     'pending', NULL, NULL, 'pending', NULL, NULL, NULL, ?26, ?27, ?27)",
            params![
                publication.id,
                publication.run_id,
                publication.flow_name,
                publication.attempt_id,
                publication.operation_key,
                publication.selection_id,
                publication.task_id,
                publication.plan_revision_id,
                publication.plan_content_hash,
                publication.packet_revision_id,
                publication.packet_content_hash,
                publication.branch,
                publication.base_branch,
                publication.workspace_path,
                publication.base_sha,
                publication.commit_sha,
                publication.test_artifact_id,
                publication.test_artifact_hash,
                publication.review_artifact_id,
                publication.review_artifact_hash,
                publication.acceptance_artifact_id,
                publication.acceptance_artifact_hash,
                publication.scope_files_json,
                publication.scope_violations_json,
                publication.evidence_json,
                if publication.draft { 1 } else { 0 },
                publication.created_at,
            ],
        )?;
        let created = tx.query_row(
            &format!("SELECT {COLUMNS} FROM remote_publications WHERE id = ?1"),
            params![publication.id],
            read_publication,
        )?;
        tx.commit()?;
        Ok((created, true))
    }

    pub fn publication_by_operation_key(&self, key: &str) -> Result<Option<RemotePublication>> {
        let conn = lock_conn!(self);
        conn.query_row(
            &format!("SELECT {COLUMNS} FROM remote_publications WHERE operation_key = ?1"),
            params![key],
            read_publication,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn publication(&self, id: &str) -> Result<Option<RemotePublication>> {
        let conn = lock_conn!(self);
        conn.query_row(
            &format!("SELECT {COLUMNS} FROM remote_publications WHERE id = ?1"),
            params![id],
            read_publication,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn latest_publication_for_run(&self, run_id: &str) -> Result<Option<RemotePublication>> {
        let conn = lock_conn!(self);
        conn.query_row(
            &format!(
                "SELECT {COLUMNS} FROM remote_publications WHERE run_id = ?1
                      ORDER BY created_at DESC, rowid DESC LIMIT 1"
            ),
            params![run_id],
            read_publication,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn publications_for_run(&self, run_id: &str) -> Result<Vec<RemotePublication>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM remote_publications WHERE run_id = ?1 ORDER BY created_at, rowid"
        ))?;
        stmt.query_map(params![run_id], read_publication)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn mark_push_running(&self, id: &str) -> Result<()> {
        self.transition_publication(id, "push", PublicationState::Running, None)
    }

    pub fn mark_push_unknown(&self, id: &str, reason: &str) -> Result<()> {
        self.transition_publication(id, "push", PublicationState::Unknown, Some(reason))
    }

    pub fn fail_push(&self, id: &str, error: &str) -> Result<()> {
        self.transition_publication(id, "push", PublicationState::Failed, Some(error))
    }

    pub fn complete_push(&self, id: &str, remote_sha: &str) -> Result<()> {
        let mut conn = lock_conn!(self);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE remote_publications
             SET push_state = 'succeeded', push_error = NULL, remote_sha = ?2,
                 pr_state = CASE WHEN pr_state = 'pending' THEN 'pending' ELSE pr_state END,
                 updated_at = ?3
             WHERE id = ?1 AND push_state IN ('pending','running','unknown','failed')",
            params![id, remote_sha, chrono::Utc::now().to_rfc3339()],
        )?;
        anyhow::ensure!(
            changed == 1,
            "publication {id} push state transition rejected"
        );
        tx.commit()?;
        Ok(())
    }

    pub fn fail_publication(&self, id: &str, stage: &str, error: &str) -> Result<()> {
        let state = PublicationState::Failed;
        self.transition_publication(id, stage, state, Some(error))
    }

    pub fn mark_pr_running(&self, id: &str) -> Result<()> {
        self.transition_publication(id, "pr", PublicationState::Running, None)
    }

    pub fn mark_pr_unknown(&self, id: &str, reason: &str) -> Result<()> {
        self.transition_publication(id, "pr", PublicationState::Unknown, Some(reason))
    }

    pub fn complete_pr(&self, id: &str, number: u64, url: &str) -> Result<()> {
        let mut conn = lock_conn!(self);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE remote_publications
             SET pr_state = 'succeeded', pr_number = ?2, pr_url = ?3, pr_error = NULL,
                 updated_at = ?4
             WHERE id = ?1 AND push_state = 'succeeded'",
            params![id, number, url, chrono::Utc::now().to_rfc3339()],
        )?;
        anyhow::ensure!(
            changed == 1,
            "publication {id} PR transition requires a succeeded push"
        );
        tx.commit()?;
        Ok(())
    }

    fn transition_publication(
        &self,
        id: &str,
        stage: &str,
        state: PublicationState,
        error: Option<&str>,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let mut conn = lock_conn!(self);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed = match stage {
            "push" => tx.execute(
                "UPDATE remote_publications SET push_state = ?2, push_error = ?3, updated_at = ?4
                 WHERE id = ?1 AND push_state != 'succeeded'",
                params![id, state.as_str(), error, now],
            )?,
            "pr" => tx.execute(
                "UPDATE remote_publications SET pr_state = ?2, pr_error = ?3, updated_at = ?4
                 WHERE id = ?1 AND push_state = 'succeeded' AND pr_state != 'succeeded'",
                params![id, state.as_str(), error, now],
            )?,
            other => anyhow::bail!("unknown publication stage '{other}'"),
        };
        anyhow::ensure!(
            changed == 1,
            "publication {id} {stage} state transition rejected"
        );
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn db() -> (Arc<Database>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open(dir.path().join("db.sqlite").to_str().unwrap()).unwrap());
        (db, dir)
    }

    fn new_publication(key: &str) -> NewPublication {
        NewPublication {
            id: format!("pub_{key}"),
            run_id: "run".into(),
            flow_name: "engineering".into(),
            attempt_id: "attempt".into(),
            operation_key: key.into(),
            selection_id: "selection".into(),
            task_id: "T-1".into(),
            plan_revision_id: "plan".into(),
            plan_content_hash: "a".repeat(64),
            packet_revision_id: "packet".into(),
            packet_content_hash: "b".repeat(64),
            branch: "tj/run-task".into(),
            base_branch: "main".into(),
            workspace_path: "/ws".into(),
            base_sha: Some("base".into()),
            commit_sha: Some("commit".into()),
            test_artifact_id: "test".into(),
            test_artifact_hash: "c".repeat(64),
            review_artifact_id: "review".into(),
            review_artifact_hash: "d".repeat(64),
            acceptance_artifact_id: "accept".into(),
            acceptance_artifact_hash: "e".repeat(64),
            scope_files_json: "[]".into(),
            scope_violations_json: "[]".into(),
            evidence_json: None,
            draft: true,
            created_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn operation_identity_is_unique_and_states_are_ordered() {
        let (db, _dir) = db();
        let (publication, created) = db.create_publication(&new_publication("op-1")).unwrap();
        assert!(created);
        assert_eq!(publication.push_state, PublicationState::Pending);
        let (again, created) = db.create_publication(&new_publication("op-1")).unwrap();
        assert!(!created);
        assert_eq!(again.id, publication.id);
        assert_eq!(db.publications_for_run("run").unwrap().len(), 1);
    }

    #[test]
    fn failed_push_cannot_be_completed_as_published() {
        let (db, _dir) = db();
        let (publication, _) = db.create_publication(&new_publication("op-2")).unwrap();
        db.mark_push_running(&publication.id).unwrap();
        db.fail_push(&publication.id, "remote rejected").unwrap();
        let error = db
            .complete_pr(&publication.id, 1, "http://forge/pr/1")
            .unwrap_err()
            .to_string();
        assert!(error.contains("succeeded push"), "{error}");
        let loaded = db.publication(&publication.id).unwrap().unwrap();
        assert_eq!(loaded.push_state, PublicationState::Failed);
        assert_eq!(loaded.pr_state, PublicationState::Pending);
        assert!(loaded.push_error.unwrap().contains("remote rejected"));
    }

    #[test]
    fn unknown_push_must_be_reconciled_before_success() {
        let (db, _dir) = db();
        let (publication, _) = db.create_publication(&new_publication("op-3")).unwrap();
        db.mark_push_running(&publication.id).unwrap();
        db.mark_push_unknown(&publication.id, "response lost")
            .unwrap();
        let loaded = db.publication(&publication.id).unwrap().unwrap();
        assert_eq!(loaded.push_state, PublicationState::Unknown);
        assert_eq!(loaded.push_error.as_deref(), Some("response lost"));
    }

    #[test]
    fn pr_requires_durable_pushed_branch_identity() {
        let (db, _dir) = db();
        let (publication, _) = db.create_publication(&new_publication("op-4")).unwrap();
        assert!(db.mark_pr_running(&publication.id).is_err());
        db.mark_push_running(&publication.id).unwrap();
        db.complete_push(&publication.id, "commit").unwrap();
        db.mark_pr_running(&publication.id).unwrap();
        db.mark_pr_unknown(&publication.id, "lost response")
            .unwrap();
        db.complete_pr(&publication.id, 7, "http://forge/pr/7")
            .unwrap();
        let loaded = db.publication(&publication.id).unwrap().unwrap();
        assert_eq!(loaded.push_state, PublicationState::Succeeded);
        assert_eq!(loaded.remote_sha.as_deref(), Some("commit"));
        assert_eq!(loaded.pr_state, PublicationState::Succeeded);
        assert_eq!(loaded.pr_number, Some(7));
        assert!(loaded.draft);
    }
}
