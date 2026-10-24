//! Durable TJ-039/TJ-040 delivery identities, immutable production decisions,
//! signed callback log and restart reconciliation.
//!
//! The external CI/deployment system executes; these rows are TurboJet's local
//! contract. A release operation key is unique before dispatch. Callback event
//! ids are unique, stale sequences are recorded as rejected audit rows, and a
//! restart turns any in-flight dispatch/deployment into `unknown` rather than
//! inferring success.

use super::*;
use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tj_delivery::{
    CallbackEvent, CiState, DecisionAction, DeploymentState, DispatchState, ObservationState,
    Recommendation, ReleaseEnvironment, ReleaseKind, ReleaseState, TransitionError,
    VerificationState, apply_event, recommendation,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryRelease {
    pub id: String,
    pub run_id: String,
    pub project_id: String,
    pub publication_id: String,
    pub selection_id: String,
    pub task_id: String,
    pub plan_revision_id: String,
    pub plan_content_hash: String,
    pub packet_revision_id: String,
    pub packet_content_hash: String,
    pub pr_number: Option<u64>,
    pub pr_url: Option<String>,
    pub commit_sha: String,
    pub base_sha: Option<String>,
    pub branch: String,
    pub environment: ReleaseEnvironment,
    pub kind: ReleaseKind,
    pub expected_revision: String,
    pub authorization_id: Option<String>,
    pub source_release_id: Option<String>,
    pub operation_key: String,
    pub workflow: String,
    pub workflow_ref: String,
    pub dispatch_state: DispatchState,
    pub external_run_id: Option<String>,
    pub external_url: Option<String>,
    pub dispatch_error: Option<String>,
    pub ci_state: CiState,
    pub ci_run_id: Option<String>,
    pub ci_url: Option<String>,
    pub deployment_state: DeploymentState,
    pub deployed_revision: Option<String>,
    pub deployment_run_id: Option<String>,
    pub deployment_url: Option<String>,
    pub deployment_detail: Option<String>,
    pub verification_state: VerificationState,
    pub verified_revision: Option<String>,
    pub verifier: Option<String>,
    pub verification_evidence_hash: Option<String>,
    pub verification_detail: Option<String>,
    pub observation_state: ObservationState,
    pub observation_evidence_hash: Option<String>,
    pub observation_detail: Option<String>,
    pub recommendation: Recommendation,
    pub recommendation_reasons_json: String,
    pub last_sequence: i64,
    pub last_error: Option<String>,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct NewDeliveryRelease {
    pub id: String,
    pub run_id: String,
    pub project_id: String,
    pub publication_id: String,
    pub selection_id: String,
    pub task_id: String,
    pub plan_revision_id: String,
    pub plan_content_hash: String,
    pub packet_revision_id: String,
    pub packet_content_hash: String,
    pub pr_number: Option<u64>,
    pub pr_url: Option<String>,
    pub commit_sha: String,
    pub base_sha: Option<String>,
    pub branch: String,
    pub environment: ReleaseEnvironment,
    pub kind: ReleaseKind,
    pub expected_revision: String,
    pub authorization_id: Option<String>,
    pub source_release_id: Option<String>,
    pub operation_key: String,
    pub workflow: String,
    pub workflow_ref: String,
    pub created_by: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryAuthorization {
    pub id: String,
    pub run_id: String,
    pub publication_id: String,
    pub decision: DecisionAction,
    pub environment: ReleaseEnvironment,
    pub kind: ReleaseKind,
    pub commit_sha: String,
    pub expected_revision: String,
    pub staging_release_id: Option<String>,
    pub staging_verification_event_id: Option<String>,
    pub staging_observation_event_id: Option<String>,
    pub source_release_id: Option<String>,
    pub task_id: String,
    pub plan_revision_id: String,
    pub plan_content_hash: String,
    pub packet_revision_id: String,
    pub packet_content_hash: String,
    pub actor_id: String,
    pub actor_name: String,
    pub reason: Option<String>,
    pub idempotency_key: String,
    pub operation_key: String,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct NewDeliveryAuthorization {
    pub id: String,
    pub run_id: String,
    pub publication_id: String,
    pub decision: DecisionAction,
    pub environment: ReleaseEnvironment,
    pub kind: ReleaseKind,
    pub commit_sha: String,
    pub expected_revision: String,
    pub staging_release_id: Option<String>,
    pub staging_verification_event_id: Option<String>,
    pub staging_observation_event_id: Option<String>,
    pub source_release_id: Option<String>,
    pub task_id: String,
    pub plan_revision_id: String,
    pub plan_content_hash: String,
    pub packet_revision_id: String,
    pub packet_content_hash: String,
    pub actor_id: String,
    pub actor_name: String,
    pub reason: Option<String>,
    pub idempotency_key: String,
    pub operation_key: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryEventRecord {
    pub id: i64,
    pub release_id: String,
    pub event_id: String,
    pub source: String,
    pub event_kind: String,
    pub sequence: i64,
    pub payload_json: String,
    pub payload_hash: String,
    pub accepted: bool,
    pub rejection_reason: Option<String>,
    pub received_at: String,
}

#[derive(Debug, Clone)]
pub struct DeliveryEventApplication {
    pub event: DeliveryEventRecord,
    pub release: DeliveryRelease,
    pub replayed: bool,
    pub accepted: bool,
    pub error: Option<String>,
}

const RELEASE_COLUMNS: &str = "id, run_id, project_id, publication_id, selection_id, task_id,
     plan_revision_id, plan_content_hash, packet_revision_id, packet_content_hash,
     pr_number, pr_url, commit_sha, base_sha, branch, environment, kind,
     expected_revision, authorization_id, source_release_id, operation_key,
     workflow, workflow_ref, dispatch_state, external_run_id, external_url,
     dispatch_error, ci_state, ci_run_id, ci_url, deployment_state,
     deployed_revision, deployment_run_id, deployment_url, deployment_detail,
     verification_state, verified_revision, verifier, verification_evidence_hash,
     verification_detail, observation_state, observation_evidence_hash,
     observation_detail, recommendation, recommendation_reasons_json,
     last_sequence, last_error, created_by, created_at, updated_at";

fn enum_value<T>(value: &str, parse: fn(&str) -> Result<T, String>) -> rusqlite::Result<T> {
    parse(value).map_err(rusqlite::Error::InvalidParameterName)
}

fn read_release(row: &rusqlite::Row<'_>) -> rusqlite::Result<DeliveryRelease> {
    Ok(DeliveryRelease {
        id: row.get(0)?,
        run_id: row.get(1)?,
        project_id: row.get(2)?,
        publication_id: row.get(3)?,
        selection_id: row.get(4)?,
        task_id: row.get(5)?,
        plan_revision_id: row.get(6)?,
        plan_content_hash: row.get(7)?,
        packet_revision_id: row.get(8)?,
        packet_content_hash: row.get(9)?,
        pr_number: row.get::<_, Option<i64>>(10)?.map(|value| value as u64),
        pr_url: row.get(11)?,
        commit_sha: row.get(12)?,
        base_sha: row.get(13)?,
        branch: row.get(14)?,
        environment: enum_value(&row.get::<_, String>(15)?, ReleaseEnvironment::parse)?,
        kind: enum_value(&row.get::<_, String>(16)?, ReleaseKind::parse)?,
        expected_revision: row.get(17)?,
        authorization_id: row.get(18)?,
        source_release_id: row.get(19)?,
        operation_key: row.get(20)?,
        workflow: row.get(21)?,
        workflow_ref: row.get(22)?,
        dispatch_state: enum_value(&row.get::<_, String>(23)?, DispatchState::parse)?,
        external_run_id: row.get(24)?,
        external_url: row.get(25)?,
        dispatch_error: row.get(26)?,
        ci_state: enum_value(&row.get::<_, String>(27)?, CiState::parse)?,
        ci_run_id: row.get(28)?,
        ci_url: row.get(29)?,
        deployment_state: enum_value(&row.get::<_, String>(30)?, DeploymentState::parse)?,
        deployed_revision: row.get(31)?,
        deployment_run_id: row.get(32)?,
        deployment_url: row.get(33)?,
        deployment_detail: row.get(34)?,
        verification_state: enum_value(&row.get::<_, String>(35)?, VerificationState::parse)?,
        verified_revision: row.get(36)?,
        verifier: row.get(37)?,
        verification_evidence_hash: row.get(38)?,
        verification_detail: row.get(39)?,
        observation_state: enum_value(&row.get::<_, String>(40)?, ObservationState::parse)?,
        observation_evidence_hash: row.get(41)?,
        observation_detail: row.get(42)?,
        recommendation: enum_value(&row.get::<_, String>(43)?, Recommendation::parse)?,
        recommendation_reasons_json: row.get(44)?,
        last_sequence: row.get(45)?,
        last_error: row.get(46)?,
        created_by: row.get(47)?,
        created_at: row.get(48)?,
        updated_at: row.get(49)?,
    })
}

const AUTH_COLUMNS: &str = "id, run_id, publication_id, decision, environment, kind, commit_sha,
     expected_revision, staging_release_id, staging_verification_event_id,
     staging_observation_event_id, source_release_id, task_id, plan_revision_id,
     plan_content_hash, packet_revision_id, packet_content_hash, actor_id,
     actor_name, reason, idempotency_key, operation_key, created_at";

fn read_authorization(row: &rusqlite::Row<'_>) -> rusqlite::Result<DeliveryAuthorization> {
    Ok(DeliveryAuthorization {
        id: row.get(0)?,
        run_id: row.get(1)?,
        publication_id: row.get(2)?,
        decision: enum_value(&row.get::<_, String>(3)?, DecisionAction::parse)?,
        environment: enum_value(&row.get::<_, String>(4)?, ReleaseEnvironment::parse)?,
        kind: enum_value(&row.get::<_, String>(5)?, ReleaseKind::parse)?,
        commit_sha: row.get(6)?,
        expected_revision: row.get(7)?,
        staging_release_id: row.get(8)?,
        staging_verification_event_id: row.get(9)?,
        staging_observation_event_id: row.get(10)?,
        source_release_id: row.get(11)?,
        task_id: row.get(12)?,
        plan_revision_id: row.get(13)?,
        plan_content_hash: row.get(14)?,
        packet_revision_id: row.get(15)?,
        packet_content_hash: row.get(16)?,
        actor_id: row.get(17)?,
        actor_name: row.get(18)?,
        reason: row.get(19)?,
        idempotency_key: row.get(20)?,
        operation_key: row.get(21)?,
        created_at: row.get(22)?,
    })
}

const EVENT_COLUMNS: &str = "id, release_id, event_id, source, event_kind, sequence, payload_json,
     payload_hash, accepted, rejection_reason, received_at";

fn read_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<DeliveryEventRecord> {
    Ok(DeliveryEventRecord {
        id: row.get(0)?,
        release_id: row.get(1)?,
        event_id: row.get(2)?,
        source: row.get(3)?,
        event_kind: row.get(4)?,
        sequence: row.get(5)?,
        payload_json: row.get(6)?,
        payload_hash: row.get(7)?,
        accepted: row.get::<_, i64>(8)? != 0,
        rejection_reason: row.get(9)?,
        received_at: row.get(10)?,
    })
}

pub(crate) fn migrate(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS delivery_releases (
            id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL,
            project_id TEXT NOT NULL,
            publication_id TEXT NOT NULL,
            selection_id TEXT NOT NULL,
            task_id TEXT NOT NULL,
            plan_revision_id TEXT NOT NULL,
            plan_content_hash TEXT NOT NULL,
            packet_revision_id TEXT NOT NULL,
            packet_content_hash TEXT NOT NULL,
            pr_number INTEGER,
            pr_url TEXT,
            commit_sha TEXT NOT NULL,
            base_sha TEXT,
            branch TEXT NOT NULL,
            environment TEXT NOT NULL,
            kind TEXT NOT NULL,
            expected_revision TEXT NOT NULL,
            authorization_id TEXT,
            source_release_id TEXT,
            operation_key TEXT NOT NULL,
            workflow TEXT NOT NULL,
            workflow_ref TEXT NOT NULL,
            dispatch_state TEXT NOT NULL,
            external_run_id TEXT,
            external_url TEXT,
            dispatch_error TEXT,
            ci_state TEXT NOT NULL,
            ci_run_id TEXT,
            ci_url TEXT,
            deployment_state TEXT NOT NULL,
            deployed_revision TEXT,
            deployment_run_id TEXT,
            deployment_url TEXT,
            deployment_detail TEXT,
            verification_state TEXT NOT NULL,
            verified_revision TEXT,
            verifier TEXT,
            verification_evidence_hash TEXT,
            verification_detail TEXT,
            observation_state TEXT NOT NULL,
            observation_evidence_hash TEXT,
            observation_detail TEXT,
            recommendation TEXT NOT NULL,
            recommendation_reasons_json TEXT NOT NULL,
            last_sequence INTEGER NOT NULL DEFAULT 0,
            last_error TEXT,
            created_by TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_delivery_releases_operation
            ON delivery_releases(operation_key);
        CREATE INDEX IF NOT EXISTS idx_delivery_releases_run
            ON delivery_releases(run_id, created_at);
        CREATE INDEX IF NOT EXISTS idx_delivery_releases_publication
            ON delivery_releases(publication_id, environment, created_at);

        CREATE TABLE IF NOT EXISTS delivery_authorizations (
            id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL,
            publication_id TEXT NOT NULL,
            decision TEXT NOT NULL,
            environment TEXT NOT NULL,
            kind TEXT NOT NULL,
            commit_sha TEXT NOT NULL,
            expected_revision TEXT NOT NULL,
            staging_release_id TEXT,
            staging_verification_event_id TEXT,
            staging_observation_event_id TEXT,
            source_release_id TEXT,
            task_id TEXT NOT NULL,
            plan_revision_id TEXT NOT NULL,
            plan_content_hash TEXT NOT NULL,
            packet_revision_id TEXT NOT NULL,
            packet_content_hash TEXT NOT NULL,
            actor_id TEXT NOT NULL,
            actor_name TEXT NOT NULL,
            reason TEXT,
            idempotency_key TEXT NOT NULL,
            operation_key TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_delivery_authorizations_operation
            ON delivery_authorizations(operation_key);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_delivery_authorizations_idempotency
            ON delivery_authorizations(idempotency_key);
        CREATE INDEX IF NOT EXISTS idx_delivery_authorizations_run
            ON delivery_authorizations(run_id, created_at);

        CREATE TABLE IF NOT EXISTS delivery_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            release_id TEXT NOT NULL,
            event_id TEXT NOT NULL,
            source TEXT NOT NULL,
            event_kind TEXT NOT NULL,
            sequence INTEGER NOT NULL,
            payload_json TEXT NOT NULL,
            payload_hash TEXT NOT NULL,
            accepted INTEGER NOT NULL,
            rejection_reason TEXT,
            received_at TEXT NOT NULL
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_delivery_events_event_id
            ON delivery_events(event_id);
        CREATE INDEX IF NOT EXISTS idx_delivery_events_release
            ON delivery_events(release_id, sequence, id);",
    )?;
    Ok(())
}

impl Database {
    pub fn create_delivery_release(
        &self,
        release: &NewDeliveryRelease,
    ) -> Result<(DeliveryRelease, bool)> {
        let mut conn = lock_conn!(self);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let Some(existing) = tx
            .query_row(
                &format!(
                    "SELECT {RELEASE_COLUMNS} FROM delivery_releases WHERE operation_key = ?1"
                ),
                params![release.operation_key],
                read_release,
            )
            .optional()?
        {
            return Ok((existing, false));
        }
        let now = release.created_at.clone();
        tx.execute(
            "INSERT INTO delivery_releases
             (id, run_id, project_id, publication_id, selection_id, task_id,
              plan_revision_id, plan_content_hash, packet_revision_id, packet_content_hash,
              pr_number, pr_url, commit_sha, base_sha, branch, environment, kind,
              expected_revision, authorization_id, source_release_id, operation_key,
              workflow, workflow_ref, dispatch_state, external_run_id, external_url,
              dispatch_error, ci_state, ci_run_id, ci_url, deployment_state,
              deployed_revision, deployment_run_id, deployment_url, deployment_detail,
              verification_state, verified_revision, verifier, verification_evidence_hash,
              verification_detail, observation_state, observation_evidence_hash,
              observation_detail, recommendation, recommendation_reasons_json,
              last_sequence, last_error, created_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                     ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, 'pending', NULL, NULL,
                     NULL, 'not_started', NULL, NULL, 'not_requested', NULL, NULL, NULL,
                     NULL, 'pending', NULL, NULL, NULL, NULL, 'pending', NULL, NULL,
                     'pending', ?24, 0, NULL, ?25, ?26, ?26)",
            params![
                release.id,
                release.run_id,
                release.project_id,
                release.publication_id,
                release.selection_id,
                release.task_id,
                release.plan_revision_id,
                release.plan_content_hash,
                release.packet_revision_id,
                release.packet_content_hash,
                release.pr_number.map(|value| value as i64),
                release.pr_url,
                release.commit_sha,
                release.base_sha,
                release.branch,
                release.environment.as_str(),
                release.kind.as_str(),
                release.expected_revision,
                release.authorization_id,
                release.source_release_id,
                release.operation_key,
                release.workflow,
                release.workflow_ref,
                serde_json::to_string(&vec!["external CI workflow has not reported yet"])
                    .unwrap_or_else(|_| "[]".into()),
                release.created_by,
                now,
            ],
        )?;
        let created = tx.query_row(
            &format!("SELECT {RELEASE_COLUMNS} FROM delivery_releases WHERE id = ?1"),
            params![release.id],
            read_release,
        )?;
        tx.commit()?;
        Ok((created, true))
    }

    pub fn delivery_release(&self, id: &str) -> Result<Option<DeliveryRelease>> {
        let conn = lock_conn!(self);
        conn.query_row(
            &format!("SELECT {RELEASE_COLUMNS} FROM delivery_releases WHERE id = ?1"),
            params![id],
            read_release,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn delivery_release_by_operation_key(&self, key: &str) -> Result<Option<DeliveryRelease>> {
        let conn = lock_conn!(self);
        conn.query_row(
            &format!("SELECT {RELEASE_COLUMNS} FROM delivery_releases WHERE operation_key = ?1"),
            params![key],
            read_release,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn delivery_releases_for_run(&self, run_id: &str) -> Result<Vec<DeliveryRelease>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(&format!(
            "SELECT {RELEASE_COLUMNS} FROM delivery_releases WHERE run_id = ?1
             ORDER BY created_at, rowid"
        ))?;
        stmt.query_map(params![run_id], read_release)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn latest_delivery_release_for_publication(
        &self,
        publication_id: &str,
        environment: ReleaseEnvironment,
        kind: ReleaseKind,
    ) -> Result<Option<DeliveryRelease>> {
        let conn = lock_conn!(self);
        conn.query_row(
            &format!(
                "SELECT {RELEASE_COLUMNS} FROM delivery_releases
                 WHERE publication_id = ?1 AND environment = ?2 AND kind = ?3
                 ORDER BY created_at DESC, rowid DESC LIMIT 1"
            ),
            params![publication_id, environment.as_str(), kind.as_str()],
            read_release,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn create_delivery_authorization(
        &self,
        authorization: &NewDeliveryAuthorization,
    ) -> Result<(DeliveryAuthorization, bool)> {
        let mut conn = lock_conn!(self);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let Some(existing) = tx
            .query_row(
                &format!(
                    "SELECT {AUTH_COLUMNS} FROM delivery_authorizations WHERE operation_key = ?1"
                ),
                params![authorization.operation_key],
                read_authorization,
            )
            .optional()?
        {
            return Ok((existing, false));
        }
        if let Some(existing) = tx
            .query_row(
                &format!(
                    "SELECT {AUTH_COLUMNS} FROM delivery_authorizations WHERE idempotency_key = ?1"
                ),
                params![authorization.idempotency_key],
                read_authorization,
            )
            .optional()?
        {
            anyhow::ensure!(
                existing.operation_key == authorization.operation_key,
                "delivery idempotency key was already used for a different exact decision"
            );
            return Ok((existing, false));
        }
        tx.execute(
            "INSERT INTO delivery_authorizations
             (id, run_id, publication_id, decision, environment, kind, commit_sha,
              expected_revision, staging_release_id, staging_verification_event_id,
              staging_observation_event_id, source_release_id, task_id, plan_revision_id,
              plan_content_hash, packet_revision_id, packet_content_hash, actor_id,
              actor_name, reason, idempotency_key, operation_key, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                     ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)",
            params![
                authorization.id,
                authorization.run_id,
                authorization.publication_id,
                authorization.decision.as_str(),
                authorization.environment.as_str(),
                authorization.kind.as_str(),
                authorization.commit_sha,
                authorization.expected_revision,
                authorization.staging_release_id,
                authorization.staging_verification_event_id,
                authorization.staging_observation_event_id,
                authorization.source_release_id,
                authorization.task_id,
                authorization.plan_revision_id,
                authorization.plan_content_hash,
                authorization.packet_revision_id,
                authorization.packet_content_hash,
                authorization.actor_id,
                authorization.actor_name,
                authorization.reason,
                authorization.idempotency_key,
                authorization.operation_key,
                authorization.created_at,
            ],
        )?;
        let created = tx.query_row(
            &format!("SELECT {AUTH_COLUMNS} FROM delivery_authorizations WHERE id = ?1"),
            params![authorization.id],
            read_authorization,
        )?;
        tx.commit()?;
        Ok((created, true))
    }

    pub fn delivery_authorization(&self, id: &str) -> Result<Option<DeliveryAuthorization>> {
        let conn = lock_conn!(self);
        conn.query_row(
            &format!("SELECT {AUTH_COLUMNS} FROM delivery_authorizations WHERE id = ?1"),
            params![id],
            read_authorization,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn delivery_authorizations_for_run(
        &self,
        run_id: &str,
    ) -> Result<Vec<DeliveryAuthorization>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(&format!(
            "SELECT {AUTH_COLUMNS} FROM delivery_authorizations WHERE run_id = ?1
             ORDER BY created_at, rowid"
        ))?;
        stmt.query_map(params![run_id], read_authorization)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn delivery_events_for_release(
        &self,
        release_id: &str,
    ) -> Result<Vec<DeliveryEventRecord>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(&format!(
            "SELECT {EVENT_COLUMNS} FROM delivery_events WHERE release_id = ?1
             ORDER BY sequence, id"
        ))?;
        stmt.query_map(params![release_id], read_event)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn delivery_events_for_run(&self, run_id: &str) -> Result<Vec<DeliveryEventRecord>> {
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(&format!(
            "SELECT e.id, e.release_id, e.event_id, e.source, e.event_kind, e.sequence,
                    e.payload_json, e.payload_hash, e.accepted, e.rejection_reason, e.received_at
             FROM delivery_events e
             JOIN delivery_releases r ON r.id = e.release_id
             WHERE r.run_id = ?1 ORDER BY e.sequence, e.id"
        ))?;
        stmt.query_map(params![run_id], read_event)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn begin_delivery_dispatch(&self, id: &str) -> Result<()> {
        let changed = lock_conn!(self).execute(
            "UPDATE delivery_releases SET dispatch_state = 'running', dispatch_error = NULL,
                    updated_at = ?2
             WHERE id = ?1 AND dispatch_state = 'pending'",
            params![id, chrono::Utc::now().to_rfc3339()],
        )?;
        anyhow::ensure!(
            changed == 1,
            "delivery release {id} is not pending dispatch"
        );
        Ok(())
    }

    pub fn complete_delivery_dispatch(
        &self,
        id: &str,
        external_run_id: Option<&str>,
        external_url: Option<&str>,
    ) -> Result<()> {
        let changed = lock_conn!(self).execute(
            "UPDATE delivery_releases
             SET dispatch_state = 'succeeded', external_run_id = ?2, external_url = ?3,
                 dispatch_error = NULL, ci_state = 'pending', last_error = NULL, updated_at = ?4
             WHERE id = ?1 AND dispatch_state IN ('pending','running','unknown')",
            params![
                id,
                external_run_id,
                external_url,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        anyhow::ensure!(
            changed == 1,
            "delivery release {id} dispatch completion rejected"
        );
        self.refresh_delivery_recommendation(id)
    }

    pub fn fail_delivery_dispatch(&self, id: &str, error: &str) -> Result<()> {
        let changed = lock_conn!(self).execute(
            "UPDATE delivery_releases
             SET dispatch_state = 'failed', dispatch_error = ?2, last_error = ?2, updated_at = ?3
             WHERE id = ?1 AND dispatch_state IN ('pending','running','unknown')",
            params![id, error, chrono::Utc::now().to_rfc3339()],
        )?;
        anyhow::ensure!(
            changed == 1,
            "delivery release {id} dispatch failure rejected"
        );
        self.refresh_delivery_recommendation(id)
    }

    pub fn mark_delivery_dispatch_unknown(&self, id: &str, reason: &str) -> Result<()> {
        let changed = lock_conn!(self).execute(
            "UPDATE delivery_releases
             SET dispatch_state = 'unknown', dispatch_error = ?2, last_error = ?2, updated_at = ?3
             WHERE id = ?1 AND dispatch_state IN ('pending','running')",
            params![id, reason, chrono::Utc::now().to_rfc3339()],
        )?;
        anyhow::ensure!(
            changed == 1,
            "delivery release {id} unknown-dispatch transition rejected"
        );
        self.refresh_delivery_recommendation(id)
    }

    /// Explicitly re-dispatch an operation whose previous dispatch definitely
    /// failed or is durably unknown. The caller must acknowledge that an
    /// unknown attempt may have reached the external system; the workflow is
    /// expected to deduplicate by the unchanged operation key. Never automatic.
    pub fn prepare_delivery_redispatch(&self, id: &str) -> Result<()> {
        let mut conn = lock_conn!(self);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let release: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT dispatch_state, external_run_id FROM delivery_releases WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((state, external_run_id)) = release else {
            anyhow::bail!("delivery release '{id}' does not exist");
        };
        anyhow::ensure!(
            matches!(state.as_str(), "failed" | "unknown"),
            "delivery release {id} is not in a failed/unknown dispatch state"
        );
        anyhow::ensure!(
            external_run_id.is_none(),
            "delivery release {id} already has an external run id; reconcile it instead of re-dispatching"
        );
        let accepted: i64 = tx.query_row(
            "SELECT COUNT(1) FROM delivery_events WHERE release_id = ?1 AND accepted = 1",
            params![id],
            |row| row.get(0),
        )?;
        anyhow::ensure!(
            accepted == 0,
            "delivery release {id} already has accepted external evidence; reconcile it instead of re-dispatching"
        );
        tx.execute(
            "UPDATE delivery_releases SET dispatch_state = 'running', dispatch_error = NULL,
                    last_error = NULL, updated_at = ?2 WHERE id = ?1",
            params![id, chrono::Utc::now().to_rfc3339()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn attach_delivery_external_run(
        &self,
        id: &str,
        external_run_id: Option<&str>,
        external_url: Option<&str>,
    ) -> Result<()> {
        let conn = lock_conn!(self);
        conn.execute(
            "UPDATE delivery_releases
             SET external_run_id = COALESCE(?2, external_run_id),
                 external_url = COALESCE(?3, external_url), updated_at = ?4
             WHERE id = ?1",
            params![
                id,
                external_run_id,
                external_url,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        Ok(())
    }

    /// Apply one authenticated callback. Duplicate event ids replay the first
    /// result. A stale sequence or rejected transition is persisted as a
    /// rejected audit row and never changes release status.
    pub fn apply_delivery_callback(
        &self,
        release_id: &str,
        event: &CallbackEvent,
        source: &str,
        raw_payload: &str,
    ) -> Result<DeliveryEventApplication> {
        let payload_hash = hex::encode(Sha256::digest(raw_payload.as_bytes()));
        let now = chrono::Utc::now().to_rfc3339();
        let mut conn = lock_conn!(self);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let release = tx
            .query_row(
                &format!("SELECT {RELEASE_COLUMNS} FROM delivery_releases WHERE id = ?1"),
                params![release_id],
                read_release,
            )
            .optional()?
            .with_context(|| format!("delivery release '{release_id}' does not exist"))?;
        if let Some(existing) = tx
            .query_row(
                &format!("SELECT {EVENT_COLUMNS} FROM delivery_events WHERE event_id = ?1"),
                params![event.event_id],
                read_event,
            )
            .optional()?
        {
            let same = existing.release_id == release_id && existing.payload_hash == payload_hash;
            let error = if same {
                existing.rejection_reason.clone()
            } else {
                Some("event_id replay with a different release or payload".into())
            };
            return Ok(DeliveryEventApplication {
                accepted: same && existing.accepted,
                replayed: true,
                event: existing,
                release,
                error,
            });
        }

        let state = release_state(&release);
        let transition = apply_event(&state, event);
        let (accepted, rejection, updated) = match transition {
            Ok(next) => (true, None, Some(next)),
            Err(error) => {
                let reason = transition_reason(&error);
                (false, Some(reason), None)
            }
        };
        let reasons_json = match &updated {
            Some(next) => {
                serde_json::to_string(&recommendation(next).1).unwrap_or_else(|_| "[]".into())
            }
            None => release.recommendation_reasons_json.clone(),
        };
        tx.execute(
            "INSERT INTO delivery_events
             (release_id, event_id, source, event_kind, sequence, payload_json,
              payload_hash, accepted, rejection_reason, received_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                release_id,
                event.event_id,
                source,
                event.kind_name(),
                event.sequence,
                raw_payload,
                payload_hash,
                if accepted { 1 } else { 0 },
                rejection,
                now,
            ],
        )?;
        if let Some(next) = updated {
            let (recommendation, _) = recommendation(&next);
            let updated_at = now.clone();
            tx.execute(
                "UPDATE delivery_releases SET
                    ci_state = ?2, ci_run_id = COALESCE(?3, ci_run_id), ci_url = COALESCE(?4, ci_url),
                    deployment_state = ?5, deployed_revision = COALESCE(?6, deployed_revision),
                    deployment_run_id = COALESCE(?7, deployment_run_id),
                    deployment_url = COALESCE(?8, deployment_url),
                    deployment_detail = COALESCE(?9, deployment_detail),
                    verification_state = ?10, verified_revision = COALESCE(?11, verified_revision),
                    verifier = COALESCE(?12, verifier),
                    verification_evidence_hash = COALESCE(?13, verification_evidence_hash),
                    verification_detail = COALESCE(?14, verification_detail),
                    observation_state = ?15,
                    observation_evidence_hash = COALESCE(?16, observation_evidence_hash),
                    observation_detail = COALESCE(?17, observation_detail),
                    recommendation = ?18, recommendation_reasons_json = ?19,
                    last_sequence = ?20, last_error = NULL, updated_at = ?21
                 WHERE id = ?1",
                params![
                    release_id, next.ci_state.as_str(),
                    match &event.kind { tj_delivery::CallbackKind::Ci { external_run_id, .. } => Some(external_run_id), _ => None },
                    match &event.kind { tj_delivery::CallbackKind::Ci { external_url, .. } => external_url.as_deref(), _ => None },
                    next.deployment_state.as_str(),
                    next.deployed_revision.clone(),
                    match &event.kind { tj_delivery::CallbackKind::Deployment { external_run_id, .. } => external_run_id.as_deref(), _ => None },
                    match &event.kind { tj_delivery::CallbackKind::Deployment { external_url, .. } => external_url.as_deref(), _ => None },
                    match &event.kind { tj_delivery::CallbackKind::Deployment { detail, .. } => detail.as_deref(), _ => None },
                    next.verification_state.as_str(), next.verified_revision.clone(),
                    match &event.kind { tj_delivery::CallbackKind::Verification { verifier, .. } => Some(verifier.as_str()), _ => None },
                    match &event.kind { tj_delivery::CallbackKind::Verification { evidence_hash, .. } => Some(evidence_hash.as_str()), _ => None },
                    match &event.kind { tj_delivery::CallbackKind::Verification { detail, .. } => detail.as_deref(), _ => None },
                    next.observation_state.as_str(),
                    match &event.kind { tj_delivery::CallbackKind::Observation { evidence_hash, .. } => Some(evidence_hash.as_str()), _ => None },
                    match &event.kind { tj_delivery::CallbackKind::Observation { detail, .. } => detail.as_deref(), _ => None },
                    recommendation.as_str(), reasons_json, next.last_sequence, updated_at,
                ],
            )?;
        } else {
            tx.execute(
                "UPDATE delivery_releases SET last_error = ?2, updated_at = ?3 WHERE id = ?1",
                params![release_id, rejection, now],
            )?;
        }
        let event_row = tx.query_row(
            &format!("SELECT {EVENT_COLUMNS} FROM delivery_events WHERE event_id = ?1"),
            params![event.event_id],
            read_event,
        )?;
        let release = tx.query_row(
            &format!("SELECT {RELEASE_COLUMNS} FROM delivery_releases WHERE id = ?1"),
            params![release_id],
            read_release,
        )?;
        tx.commit()?;
        let error = event_row.rejection_reason.clone();
        Ok(DeliveryEventApplication {
            accepted,
            replayed: false,
            event: event_row,
            release,
            error,
        })
    }

    /// Restart reconciliation. Any dispatch/deployment that was in flight is
    /// durably `unknown`; a late authenticated callback or explicit poll may
    /// later prove a result, but TurboJet never infers success from a restart.
    pub fn reconcile_delivery_startup(&self) -> Result<usize> {
        let mut conn = lock_conn!(self);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let releases: Vec<DeliveryRelease> = {
            let mut stmt = tx.prepare(&format!(
                "SELECT {RELEASE_COLUMNS} FROM delivery_releases
                 WHERE dispatch_state IN ('pending','running')
                    OR deployment_state IN ('pending','running')"
            ))?;
            stmt.query_map([], read_release)?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let count = releases.len();
        let now = chrono::Utc::now().to_rfc3339();
        for mut release in releases {
            if matches!(
                release.dispatch_state,
                DispatchState::Pending | DispatchState::Running
            ) {
                release.dispatch_state = DispatchState::Unknown;
                release.dispatch_error = Some(
                    "process restarted during external dispatch; external state is unknown and reconciliation is required"
                        .into(),
                );
            }
            if matches!(
                release.deployment_state,
                DeploymentState::Pending | DeploymentState::Running
            ) {
                release.deployment_state = DeploymentState::Unknown;
                release.deployment_detail =
                    Some("process restarted during deployment; no success is inferred".into());
            }
            if matches!(release.ci_state, CiState::Pending | CiState::Running) {
                release.ci_state = CiState::Unknown;
            }
            let (rec, reasons) = recommendation(&release_state(&release));
            tx.execute(
                "UPDATE delivery_releases SET dispatch_state = ?2, dispatch_error = ?3,
                    ci_state = ?4, deployment_state = ?5, deployment_detail = ?6,
                    recommendation = ?7, recommendation_reasons_json = ?8, updated_at = ?9
                 WHERE id = ?1",
                params![
                    release.id,
                    release.dispatch_state.as_str(),
                    release.dispatch_error,
                    release.ci_state.as_str(),
                    release.deployment_state.as_str(),
                    release.deployment_detail,
                    rec.as_str(),
                    serde_json::to_string(&reasons).unwrap_or_else(|_| "[]".into()),
                    now,
                ],
            )?;
        }
        tx.commit()?;
        Ok(count)
    }

    fn refresh_delivery_recommendation(&self, id: &str) -> Result<()> {
        let mut conn = lock_conn!(self);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let release = tx
            .query_row(
                &format!("SELECT {RELEASE_COLUMNS} FROM delivery_releases WHERE id = ?1"),
                params![id],
                read_release,
            )
            .optional()?
            .with_context(|| format!("delivery release '{id}' does not exist"))?;
        let (rec, reasons) = recommendation(&release_state(&release));
        tx.execute(
            "UPDATE delivery_releases SET recommendation = ?2, recommendation_reasons_json = ?3,
                    updated_at = ?4 WHERE id = ?1",
            params![
                id,
                rec.as_str(),
                serde_json::to_string(&reasons)?,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        tx.commit()?;
        Ok(())
    }
}

pub fn release_state(release: &DeliveryRelease) -> ReleaseState {
    ReleaseState {
        kind: release.kind,
        environment: release.environment,
        expected_revision: release.expected_revision.clone(),
        dispatch_state: release.dispatch_state,
        ci_state: release.ci_state,
        deployment_state: release.deployment_state,
        verification_state: release.verification_state,
        observation_state: release.observation_state,
        deployed_revision: release.deployed_revision.clone(),
        verified_revision: release.verified_revision.clone(),
        last_sequence: release.last_sequence,
    }
}

pub fn transition_reason(error: &TransitionError) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tj_delivery::CallbackKind;

    fn db() -> (std::sync::Arc<Database>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = std::sync::Arc::new(
            Database::open(dir.path().join("db.sqlite").to_str().unwrap()).unwrap(),
        );
        (db, dir)
    }

    fn new_release(key: &str, env: ReleaseEnvironment, kind: ReleaseKind) -> NewDeliveryRelease {
        NewDeliveryRelease {
            id: format!("rel_{key}"),
            run_id: "run".into(),
            project_id: "project".into(),
            publication_id: "pub".into(),
            selection_id: "sel".into(),
            task_id: "T-1".into(),
            plan_revision_id: "plan".into(),
            plan_content_hash: "a".repeat(64),
            packet_revision_id: "pkt".into(),
            packet_content_hash: "b".repeat(64),
            pr_number: Some(7),
            pr_url: Some("http://forge/pr/7".into()),
            commit_sha: "c".repeat(40),
            base_sha: Some("base".into()),
            branch: "tj/task".into(),
            environment: env,
            kind,
            expected_revision: if kind == ReleaseKind::Rollback {
                "d".repeat(40)
            } else {
                "c".repeat(40)
            },
            authorization_id: None,
            source_release_id: None,
            operation_key: format!("op-{key}"),
            workflow: "delivery.yml".into(),
            workflow_ref: "main".into(),
            created_by: "alice".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn callback(event_id: &str, sequence: i64, kind: CallbackKind) -> CallbackEvent {
        CallbackEvent {
            event_id: event_id.into(),
            sequence,
            source: "forgejo-actions".into(),
            occurred_at: None,
            kind,
        }
    }

    #[test]
    fn release_identity_is_unique_and_restart_never_infers_success() {
        let (db, _dir) = db();
        let (release, created) = db
            .create_delivery_release(&new_release(
                "one",
                ReleaseEnvironment::Staging,
                ReleaseKind::Deploy,
            ))
            .unwrap();
        assert!(created);
        let (again, created) = db
            .create_delivery_release(&new_release(
                "one",
                ReleaseEnvironment::Staging,
                ReleaseKind::Deploy,
            ))
            .unwrap();
        assert!(!created);
        assert_eq!(again.id, release.id);
        db.begin_delivery_dispatch(&release.id).unwrap();
        db.complete_delivery_dispatch(&release.id, Some("42"), None)
            .unwrap();
        let running = callback(
            "started",
            1,
            CallbackKind::Deployment {
                environment: ReleaseEnvironment::Staging,
                status: DeploymentState::Running,
                revision: "c".repeat(40),
                external_run_id: Some("job-1".into()),
                external_url: None,
                detail: None,
            },
        );
        db.apply_delivery_callback(&release.id, &running, "forgejo-actions", "deploy-started")
            .unwrap();
        let count = db.reconcile_delivery_startup().unwrap();
        assert_eq!(count, 1);
        let loaded = db.delivery_release(&release.id).unwrap().unwrap();
        assert_eq!(loaded.dispatch_state, DispatchState::Succeeded);
        assert_eq!(loaded.deployment_state, DeploymentState::Unknown);
        assert_ne!(loaded.recommendation, Recommendation::Ready);
    }

    #[test]
    fn callbacks_are_idempotent_and_stale_sequences_are_rejected_durably() {
        let (db, _dir) = db();
        let (release, _) = db
            .create_delivery_release(&new_release(
                "two",
                ReleaseEnvironment::Staging,
                ReleaseKind::Deploy,
            ))
            .unwrap();
        let body =
            serde_json::json!({"event_id":"e1","sequence":2,"source":"forgejo-actions","type":"ci",
            "status":"running","revision":"c".repeat(40),"external_run_id":"42"})
            .to_string();
        let event: CallbackEvent = serde_json::from_str(&body).unwrap();
        let first = db
            .apply_delivery_callback(&release.id, &event, "forgejo-actions", &body)
            .unwrap();
        assert!(first.accepted);
        let replay = db
            .apply_delivery_callback(&release.id, &event, "forgejo-actions", &body)
            .unwrap();
        assert!(replay.replayed);
        assert!(replay.accepted);
        let stale_body =
            serde_json::json!({"event_id":"e0","sequence":1,"source":"forgejo-actions","type":"ci",
            "status":"succeeded","revision":"c".repeat(40),"external_run_id":"42"})
            .to_string();
        let stale: CallbackEvent = serde_json::from_str(&stale_body).unwrap();
        let rejected = db
            .apply_delivery_callback(&release.id, &stale, "forgejo-actions", &stale_body)
            .unwrap();
        assert!(!rejected.accepted);
        assert!(rejected.error.unwrap().contains("stale"));
        let loaded = db.delivery_release(&release.id).unwrap().unwrap();
        assert_eq!(loaded.ci_state, CiState::Running);
        assert_eq!(
            db.delivery_events_for_release(&release.id).unwrap().len(),
            2
        );
    }

    #[test]
    fn callback_event_id_is_cross_release_nonce_not_reusable() {
        let (db, _dir) = db();
        let (first, _) = db
            .create_delivery_release(&new_release(
                "cross-a",
                ReleaseEnvironment::Staging,
                ReleaseKind::Deploy,
            ))
            .unwrap();
        let (second, _) = db
            .create_delivery_release(&new_release(
                "cross-b",
                ReleaseEnvironment::Staging,
                ReleaseKind::Deploy,
            ))
            .unwrap();
        let body = serde_json::json!({"event_id":"same-id","sequence":1,"source":"forgejo-actions",
            "type":"ci","status":"succeeded","revision":"c".repeat(40),"external_run_id":"42"})
        .to_string();
        let event: CallbackEvent = serde_json::from_str(&body).unwrap();
        let accepted = db
            .apply_delivery_callback(&first.id, &event, "forgejo-actions", &body)
            .unwrap();
        assert!(accepted.accepted);
        let moved = db
            .apply_delivery_callback(&second.id, &event, "forgejo-actions", &body)
            .unwrap();
        assert!(!moved.accepted);
        assert!(moved.replayed);
        assert!(moved.error.unwrap().contains("different release"));
        let second_state = db.delivery_release(&second.id).unwrap().unwrap();
        assert_eq!(second_state.last_sequence, 0);
        assert_eq!(second_state.ci_state, CiState::NotStarted);
    }

    #[test]
    fn failed_ci_and_unknown_deployment_block_recommendation() {
        let (db, _dir) = db();
        let (release, _) = db
            .create_delivery_release(&new_release(
                "three",
                ReleaseEnvironment::Staging,
                ReleaseKind::Deploy,
            ))
            .unwrap();
        let sha = "c".repeat(40);
        let failed = callback(
            "e1",
            1,
            CallbackKind::Ci {
                status: CiState::Failed,
                revision: sha.clone(),
                external_run_id: "42".into(),
                external_url: None,
                workflow: None,
                detail: Some("tests failed".into()),
            },
        );
        db.apply_delivery_callback(&release.id, &failed, "forgejo-actions", "failed-ci")
            .unwrap();
        let loaded = db.delivery_release(&release.id).unwrap().unwrap();
        assert_eq!(loaded.recommendation, Recommendation::Blocked);
        let unknown = callback(
            "e2",
            2,
            CallbackKind::Deployment {
                environment: ReleaseEnvironment::Staging,
                status: DeploymentState::Unknown,
                revision: sha,
                external_run_id: None,
                external_url: None,
                detail: None,
            },
        );
        db.apply_delivery_callback(&release.id, &unknown, "forgejo-actions", "unknown-deploy")
            .unwrap();
        let loaded = db.delivery_release(&release.id).unwrap().unwrap();
        assert_eq!(loaded.deployment_state, DeploymentState::Unknown);
        assert_eq!(loaded.recommendation, Recommendation::Blocked);
    }

    #[test]
    fn production_authorization_is_immutable_and_idempotent() {
        let (db, _dir) = db();
        let authorization = NewDeliveryAuthorization {
            id: "auth-1".into(),
            run_id: "run".into(),
            publication_id: "pub".into(),
            decision: DecisionAction::Authorize,
            environment: ReleaseEnvironment::Production,
            kind: ReleaseKind::Deploy,
            commit_sha: "c".repeat(40),
            expected_revision: "c".repeat(40),
            staging_release_id: Some("rel_staging".into()),
            staging_verification_event_id: Some("v1".into()),
            staging_observation_event_id: Some("o1".into()),
            source_release_id: None,
            task_id: "T-1".into(),
            plan_revision_id: "plan".into(),
            plan_content_hash: "a".repeat(64),
            packet_revision_id: "pkt".into(),
            packet_content_hash: "b".repeat(64),
            actor_id: "alice".into(),
            actor_name: "Alice".into(),
            reason: Some("staging verified".into()),
            idempotency_key: "decision-key".into(),
            operation_key: "decision-op".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
        };
        let (created, is_new) = db.create_delivery_authorization(&authorization).unwrap();
        assert!(is_new);
        let (replayed, is_new) = db.create_delivery_authorization(&authorization).unwrap();
        assert!(!is_new);
        assert_eq!(created.id, replayed.id);
        let mut changed = authorization.clone();
        changed.id = "auth-2".into();
        changed.operation_key = "decision-op-2".into();
        assert!(db.create_delivery_authorization(&changed).is_err());
    }
    #[test]
    fn redispatch_is_explicit_and_refuses_existing_evidence_or_external_run() {
        let (db, _dir) = db();
        let (release, _) = db
            .create_delivery_release(&new_release(
                "retry",
                ReleaseEnvironment::Staging,
                ReleaseKind::Deploy,
            ))
            .unwrap();
        assert!(
            db.prepare_delivery_redispatch(&release.id).is_err(),
            "pending is not retryable"
        );
        db.begin_delivery_dispatch(&release.id).unwrap();
        db.fail_delivery_dispatch(&release.id, "workflow not found")
            .unwrap();
        db.prepare_delivery_redispatch(&release.id).unwrap();
        let loaded = db.delivery_release(&release.id).unwrap().unwrap();
        assert_eq!(loaded.dispatch_state, DispatchState::Running);
        db.complete_delivery_dispatch(&release.id, Some("42"), None)
            .unwrap();
        assert!(
            db.prepare_delivery_redispatch(&release.id).is_err(),
            "external run requires reconciliation"
        );
        let (other, _) = db
            .create_delivery_release(&new_release(
                "retry-2",
                ReleaseEnvironment::Staging,
                ReleaseKind::Deploy,
            ))
            .unwrap();
        db.begin_delivery_dispatch(&other.id).unwrap();
        db.mark_delivery_dispatch_unknown(&other.id, "timeout")
            .unwrap();
        let evidence = callback(
            "retry-evidence",
            1,
            CallbackKind::Ci {
                status: CiState::Running,
                revision: "c".repeat(40),
                external_run_id: "43".into(),
                external_url: None,
                workflow: None,
                detail: None,
            },
        );
        db.apply_delivery_callback(&other.id, &evidence, "forgejo-actions", "evidence")
            .unwrap();
        assert!(
            db.prepare_delivery_redispatch(&other.id).is_err(),
            "accepted evidence requires reconciliation"
        );
    }
}
