//! Local delivery contracts for TJ-039/TJ-040.
//!
//! Delivery is deliberately *not* executed by TurboJet. The adapter dispatches
//! an external Forgejo-Actions-compatible workflow and signed callbacks (or an
//! explicit poll) carry CI, deployment, independent verification and
//! observation evidence back. This module owns the pure state machine so
//! duplicate, stale, out-of-order and mismatched-revision callbacks are decided
//! deterministically before any durable row is updated.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const DELIVERY_SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseEnvironment {
    Staging,
    Production,
}

impl ReleaseEnvironment {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Staging => "staging",
            Self::Production => "production",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "staging" => Ok(Self::Staging),
            "production" => Ok(Self::Production),
            other => Err(format!("unknown delivery environment '{other}'")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseKind {
    Deploy,
    Rollback,
}

impl ReleaseKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deploy => "deploy",
            Self::Rollback => "rollback",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "deploy" => Ok(Self::Deploy),
            "rollback" => Ok(Self::Rollback),
            other => Err(format!("unknown delivery release kind '{other}'")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchState {
    Pending,
    Running,
    Succeeded,
    Failed,
    Unknown,
}

impl DispatchState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "unknown" => Ok(Self::Unknown),
            other => Err(format!("unknown dispatch state '{other}'")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CiState {
    NotStarted,
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Unknown,
}

impl CiState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotStarted => "not_started",
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "not_started" => Ok(Self::NotStarted),
            "pending" | "queued" => Ok(Self::Pending),
            "running" | "in_progress" => Ok(Self::Running),
            "succeeded" | "success" => Ok(Self::Succeeded),
            "failed" | "failure" | "timed_out" => Ok(Self::Failed),
            "cancelled" | "canceled" => Ok(Self::Cancelled),
            "unknown" => Ok(Self::Unknown),
            other => Err(format!("unknown CI state '{other}'")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentState {
    NotRequested,
    Pending,
    Running,
    Succeeded,
    Failed,
    Unknown,
}

impl DeploymentState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotRequested => "not_requested",
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "not_requested" => Ok(Self::NotRequested),
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "unknown" => Ok(Self::Unknown),
            other => Err(format!("unknown deployment state '{other}'")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationState {
    Pending,
    Passed,
    Failed,
    Unknown,
}

impl VerificationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "pending" => Ok(Self::Pending),
            "passed" => Ok(Self::Passed),
            "failed" => Ok(Self::Failed),
            "unknown" => Ok(Self::Unknown),
            other => Err(format!("unknown verification state '{other}'")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationState {
    Pending,
    Healthy,
    Degraded,
    Failed,
    Unknown,
}

impl ObservationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "pending" => Ok(Self::Pending),
            "healthy" => Ok(Self::Healthy),
            "degraded" => Ok(Self::Degraded),
            "failed" => Ok(Self::Failed),
            "unknown" => Ok(Self::Unknown),
            other => Err(format!("unknown observation state '{other}'")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Recommendation {
    Pending,
    Ready,
    Blocked,
}

impl Recommendation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ready => "ready",
            Self::Blocked => "blocked",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "pending" => Ok(Self::Pending),
            "ready" => Ok(Self::Ready),
            "blocked" => Ok(Self::Blocked),
            other => Err(format!("unknown release recommendation '{other}'")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionAction {
    Authorize,
    Reject,
}

impl DecisionAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Authorize => "authorize",
            Self::Reject => "reject",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "authorize" => Ok(Self::Authorize),
            "reject" => Ok(Self::Reject),
            other => Err(format!("unknown delivery decision action '{other}'")),
        }
    }
}

/// Immutable identity of one external delivery operation. Every field is
/// derived from durable TurboJet state before any external effect.
#[derive(Debug, Clone)]
pub struct ReleaseIdentity<'a> {
    pub run_id: &'a str,
    pub project_id: &'a str,
    pub publication_id: &'a str,
    pub selection_id: &'a str,
    pub task_id: &'a str,
    pub plan_revision_id: &'a str,
    pub plan_content_hash: &'a str,
    pub packet_revision_id: &'a str,
    pub packet_content_hash: &'a str,
    pub pr_number: Option<u64>,
    pub commit_sha: &'a str,
    pub base_sha: &'a str,
    pub branch: &'a str,
    pub environment: ReleaseEnvironment,
    pub kind: ReleaseKind,
    pub expected_revision: &'a str,
    pub authorization_id: Option<&'a str>,
    pub source_release_id: Option<&'a str>,
}

pub fn release_operation_key(identity: &ReleaseIdentity<'_>) -> String {
    let mut hasher = Sha256::new();
    let mut feed = |value: &str| {
        hasher.update(value.as_bytes());
        hasher.update([0u8]);
    };
    feed("tj-delivery-v1");
    feed(identity.run_id);
    feed(identity.project_id);
    feed(identity.publication_id);
    feed(identity.selection_id);
    feed(identity.task_id);
    feed(identity.plan_revision_id);
    feed(identity.plan_content_hash);
    feed(identity.packet_revision_id);
    feed(identity.packet_content_hash);
    feed(&identity.pr_number.unwrap_or_default().to_string());
    feed(identity.commit_sha);
    feed(identity.base_sha);
    feed(identity.branch);
    feed(identity.environment.as_str());
    feed(identity.kind.as_str());
    feed(identity.expected_revision);
    feed(identity.authorization_id.unwrap_or(""));
    feed(identity.source_release_id.unwrap_or(""));
    format!("deliveryop:{}", hex::encode(hasher.finalize()))
}

pub fn authorization_operation_key(
    run_id: &str,
    publication_id: &str,
    decision: DecisionAction,
    kind: ReleaseKind,
    expected_revision: &str,
    staging_release_id: Option<&str>,
    source_release_id: Option<&str>,
    idempotency_key: &str,
) -> String {
    let mut hasher = Sha256::new();
    for value in [
        "tj-delivery-decision-v1",
        run_id,
        publication_id,
        decision.as_str(),
        kind.as_str(),
        expected_revision,
        staging_release_id.unwrap_or(""),
        source_release_id.unwrap_or(""),
        idempotency_key,
    ] {
        hasher.update(value.as_bytes());
        hasher.update([0u8]);
    }
    format!("deliverydecision:{}", hex::encode(hasher.finalize()))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CallbackKind {
    Ci {
        status: CiState,
        revision: String,
        external_run_id: String,
        #[serde(default)]
        external_url: Option<String>,
        #[serde(default)]
        workflow: Option<String>,
        #[serde(default)]
        detail: Option<String>,
    },
    Deployment {
        environment: ReleaseEnvironment,
        status: DeploymentState,
        revision: String,
        #[serde(default)]
        external_run_id: Option<String>,
        #[serde(default)]
        external_url: Option<String>,
        #[serde(default)]
        detail: Option<String>,
    },
    Verification {
        environment: ReleaseEnvironment,
        status: VerificationState,
        revision: String,
        verifier: String,
        evidence_hash: String,
        #[serde(default)]
        detail: Option<String>,
    },
    Observation {
        environment: ReleaseEnvironment,
        status: ObservationState,
        revision: String,
        evidence_hash: String,
        #[serde(default)]
        detail: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CallbackEvent {
    pub event_id: String,
    /// Externally assigned monotonic sequence for this release stream. A lower
    /// or equal sequence is rejected as stale and never mutates state.
    pub sequence: i64,
    pub source: String,
    #[serde(default)]
    pub occurred_at: Option<String>,
    #[serde(flatten)]
    pub kind: CallbackKind,
}

impl CallbackEvent {
    pub fn revision(&self) -> &str {
        match &self.kind {
            CallbackKind::Ci { revision, .. }
            | CallbackKind::Deployment { revision, .. }
            | CallbackKind::Verification { revision, .. }
            | CallbackKind::Observation { revision, .. } => revision,
        }
    }

    pub fn environment(&self) -> Option<ReleaseEnvironment> {
        match &self.kind {
            CallbackKind::Ci { .. } => None,
            CallbackKind::Deployment { environment, .. }
            | CallbackKind::Verification { environment, .. }
            | CallbackKind::Observation { environment, .. } => Some(*environment),
        }
    }

    pub fn kind_name(&self) -> &'static str {
        match &self.kind {
            CallbackKind::Ci { .. } => "ci",
            CallbackKind::Deployment { .. } => "deployment",
            CallbackKind::Verification { .. } => "verification",
            CallbackKind::Observation { .. } => "observation",
        }
    }
}

/// Durable status subset used by the pure transition. The database adds
/// external identities and provenance around this snapshot.
#[derive(Debug, Clone)]
pub struct ReleaseState {
    pub kind: ReleaseKind,
    pub environment: ReleaseEnvironment,
    pub expected_revision: String,
    pub dispatch_state: DispatchState,
    pub ci_state: CiState,
    pub deployment_state: DeploymentState,
    pub verification_state: VerificationState,
    pub observation_state: ObservationState,
    pub deployed_revision: Option<String>,
    pub verified_revision: Option<String>,
    pub last_sequence: i64,
}

#[derive(Debug, Clone, PartialEq, Error)]
pub enum TransitionError {
    #[error("stale/out-of-order callback sequence {received}; last applied sequence is {last}")]
    StaleSequence { last: i64, received: i64 },
    #[error(
        "callback revision '{received}' does not match the exact release revision '{expected}'"
    )]
    RevisionMismatch { expected: String, received: String },
    #[error("callback targets environment '{received}', release is '{expected}'")]
    EnvironmentMismatch { expected: String, received: String },
    #[error("CI callbacks do not apply to a rollback release")]
    CiNotApplicable,
    #[error("invalid callback: {0}")]
    Invalid(String),
}

/// Apply one authenticated callback. The caller persists the event only after
/// this function succeeds; rejected events may still be recorded as rejected
/// audit rows. A successful deployment/verification is accepted only when the
/// callback names the exact expected revision.
pub fn apply_event(
    state: &ReleaseState,
    event: &CallbackEvent,
) -> Result<ReleaseState, TransitionError> {
    if event.event_id.trim().is_empty() {
        return Err(TransitionError::Invalid("event_id is required".into()));
    }
    if event.sequence <= 0 {
        return Err(TransitionError::Invalid("sequence must be positive".into()));
    }
    if event.sequence <= state.last_sequence {
        return Err(TransitionError::StaleSequence {
            last: state.last_sequence,
            received: event.sequence,
        });
    }
    let expected = state.expected_revision.trim();
    if event.revision().trim() != expected {
        return Err(TransitionError::RevisionMismatch {
            expected: expected.to_string(),
            received: event.revision().to_string(),
        });
    }
    if let Some(environment) = event.environment()
        && environment != state.environment
    {
        return Err(TransitionError::EnvironmentMismatch {
            expected: state.environment.as_str().into(),
            received: environment.as_str().into(),
        });
    }

    let mut next = state.clone();
    next.last_sequence = event.sequence;
    match &event.kind {
        CallbackKind::Ci { status, .. } => {
            if state.kind != ReleaseKind::Deploy {
                return Err(TransitionError::CiNotApplicable);
            }
            next.ci_state = *status;
        }
        CallbackKind::Deployment {
            status, revision, ..
        } => {
            next.deployment_state = *status;
            if *status == DeploymentState::Succeeded {
                next.deployed_revision = Some(revision.clone());
            }
        }
        CallbackKind::Verification {
            status,
            revision,
            verifier,
            evidence_hash,
            ..
        } => {
            if verifier.trim().is_empty() {
                return Err(TransitionError::Invalid(
                    "verification requires an independent verifier identity".into(),
                ));
            }
            if evidence_hash.trim().len() < 16 {
                return Err(TransitionError::Invalid(
                    "verification requires a non-trivial evidence hash".into(),
                ));
            }
            next.verification_state = *status;
            if *status == VerificationState::Passed {
                next.verified_revision = Some(revision.clone());
            }
        }
        CallbackKind::Observation {
            status,
            evidence_hash,
            ..
        } => {
            if evidence_hash.trim().len() < 16 {
                return Err(TransitionError::Invalid(
                    "observation requires a non-trivial evidence hash".into(),
                ));
            }
            next.observation_state = *status;
        }
    }
    Ok(next)
}

/// Deterministic release recommendation. CI and verification failures are
/// blocking, not warnings. Unknown states never become success by omission.
pub fn recommendation(state: &ReleaseState) -> (Recommendation, Vec<String>) {
    let mut blocked = Vec::new();
    let mut pending = Vec::new();

    match state.dispatch_state {
        DispatchState::Failed => blocked.push("external workflow dispatch failed".into()),
        DispatchState::Unknown => pending.push(
            "external workflow dispatch is unknown; signed callback or authoritative polling is required".into(),
        ),
        DispatchState::Pending | DispatchState::Running => {
            pending.push(format!("external workflow dispatch is {}", state.dispatch_state.as_str()))
        }
        DispatchState::Succeeded => {}
    }

    if state.kind == ReleaseKind::Deploy {
        match state.ci_state {
            CiState::Failed | CiState::Cancelled => {
                blocked.push(format!("CI is {}", state.ci_state.as_str()));
            }
            CiState::Succeeded => {}
            other => pending.push(format!("CI is {}", other.as_str())),
        }
    }

    match state.deployment_state {
        DeploymentState::Failed => blocked.push("deployment failed".into()),
        DeploymentState::Unknown => pending
            .push("deployment status is unknown; authoritative reconciliation is required".into()),
        DeploymentState::Pending | DeploymentState::NotRequested => {
            pending.push(format!("deployment is {}", state.deployment_state.as_str()))
        }
        DeploymentState::Running => pending.push("deployment is running".into()),
        DeploymentState::Succeeded => {
            if state.deployed_revision.as_deref() != Some(state.expected_revision.as_str()) {
                blocked.push("deployment did not report the exact release revision".into());
            }
        }
    }

    match state.verification_state {
        VerificationState::Failed => blocked.push("independent verification failed".into()),
        VerificationState::Unknown => pending.push("verification status is unknown".into()),
        VerificationState::Pending => pending.push("independent verification is pending".into()),
        VerificationState::Passed => {
            if state.verified_revision.as_deref() != Some(state.expected_revision.as_str()) {
                blocked.push("verification did not report the exact release revision".into());
            }
        }
    }

    match state.observation_state {
        ObservationState::Failed | ObservationState::Degraded => blocked.push(format!(
            "observation is {}",
            state.observation_state.as_str()
        )),
        ObservationState::Unknown => pending.push("observation status is unknown".into()),
        ObservationState::Pending => pending.push("observation is pending".into()),
        ObservationState::Healthy => {}
    }

    if !blocked.is_empty() {
        let mut reasons = blocked;
        reasons.extend(pending);
        return (Recommendation::Blocked, reasons);
    }
    if !pending.is_empty() {
        return (Recommendation::Pending, pending);
    }
    (
        Recommendation::Ready,
        vec![
            "CI succeeded for the exact commit".into(),
            "deployment and independent verification reported the exact release revision".into(),
            "health observation is healthy".into(),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> ReleaseState {
        ReleaseState {
            kind: ReleaseKind::Deploy,
            environment: ReleaseEnvironment::Staging,
            expected_revision: "a".repeat(40),
            dispatch_state: DispatchState::Succeeded,
            ci_state: CiState::NotStarted,
            deployment_state: DeploymentState::NotRequested,
            verification_state: VerificationState::Pending,
            observation_state: ObservationState::Pending,
            deployed_revision: None,
            verified_revision: None,
            last_sequence: 0,
        }
    }

    fn callback(sequence: i64, kind: CallbackKind) -> CallbackEvent {
        CallbackEvent {
            event_id: format!("evt-{sequence}"),
            sequence,
            source: "forgejo-actions".into(),
            occurred_at: None,
            kind,
        }
    }

    fn succeed(state: &ReleaseState) -> ReleaseState {
        let sha = state.expected_revision.clone();
        let ci = apply_event(
            state,
            &callback(
                1,
                CallbackKind::Ci {
                    status: CiState::Succeeded,
                    revision: sha.clone(),
                    external_run_id: "run".into(),
                    external_url: None,
                    workflow: None,
                    detail: None,
                },
            ),
        )
        .unwrap();
        let deployed = apply_event(
            &ci,
            &callback(
                2,
                CallbackKind::Deployment {
                    environment: ReleaseEnvironment::Staging,
                    status: DeploymentState::Succeeded,
                    revision: sha.clone(),
                    external_run_id: None,
                    external_url: None,
                    detail: None,
                },
            ),
        )
        .unwrap();
        let verified = apply_event(
            &deployed,
            &callback(
                3,
                CallbackKind::Verification {
                    environment: ReleaseEnvironment::Staging,
                    status: VerificationState::Passed,
                    revision: sha.clone(),
                    verifier: "health-probe".into(),
                    evidence_hash: "e".repeat(64),
                    detail: None,
                },
            ),
        )
        .unwrap();
        apply_event(
            &verified,
            &callback(
                4,
                CallbackKind::Observation {
                    environment: ReleaseEnvironment::Staging,
                    status: ObservationState::Healthy,
                    revision: sha,
                    evidence_hash: "f".repeat(64),
                    detail: None,
                },
            ),
        )
        .unwrap()
    }

    #[test]
    fn stale_and_out_of_order_callbacks_are_rejected_without_state_change() {
        let state = state();
        let accepted = apply_event(
            &state,
            &callback(
                2,
                CallbackKind::Ci {
                    status: CiState::Running,
                    revision: state.expected_revision.clone(),
                    external_run_id: "run".into(),
                    external_url: None,
                    workflow: None,
                    detail: None,
                },
            ),
        )
        .unwrap();
        let error = apply_event(
            &accepted,
            &callback(
                1,
                CallbackKind::Ci {
                    status: CiState::Succeeded,
                    revision: state.expected_revision.clone(),
                    external_run_id: "run".into(),
                    external_url: None,
                    workflow: None,
                    detail: None,
                },
            ),
        )
        .unwrap_err();
        assert!(matches!(error, TransitionError::StaleSequence { .. }));
        assert_eq!(accepted.ci_state, CiState::Running);
    }

    #[test]
    fn success_requires_exact_revision_and_all_evidence() {
        let state = state();
        let mut ready = succeed(&state);
        assert_eq!(recommendation(&ready).0, Recommendation::Ready);
        ready.observation_state = ObservationState::Unknown;
        assert_eq!(recommendation(&ready).0, Recommendation::Pending);
        ready.ci_state = CiState::Failed;
        assert_eq!(recommendation(&ready).0, Recommendation::Blocked);
    }

    #[test]
    fn unknown_deployment_never_recommends_release() {
        let state = state();
        let state = apply_event(
            &state,
            &callback(
                1,
                CallbackKind::Ci {
                    status: CiState::Succeeded,
                    revision: state.expected_revision.clone(),
                    external_run_id: "run".into(),
                    external_url: None,
                    workflow: None,
                    detail: None,
                },
            ),
        )
        .unwrap();
        let state = apply_event(
            &state,
            &callback(
                2,
                CallbackKind::Deployment {
                    environment: ReleaseEnvironment::Staging,
                    status: DeploymentState::Unknown,
                    revision: state.expected_revision.clone(),
                    external_run_id: None,
                    external_url: None,
                    detail: Some("response lost".into()),
                },
            ),
        )
        .unwrap();
        assert_eq!(state.deployment_state, DeploymentState::Unknown);
        assert_eq!(recommendation(&state).0, Recommendation::Pending);
    }

    #[test]
    fn mismatched_revision_and_environment_are_rejected() {
        let state = state();
        let error = apply_event(
            &state,
            &callback(
                1,
                CallbackKind::Deployment {
                    environment: ReleaseEnvironment::Production,
                    status: DeploymentState::Succeeded,
                    revision: "b".repeat(40),
                    external_run_id: None,
                    external_url: None,
                    detail: None,
                },
            ),
        )
        .unwrap_err();
        assert!(matches!(error, TransitionError::RevisionMismatch { .. }));
        let error = apply_event(
            &state,
            &callback(
                1,
                CallbackKind::Deployment {
                    environment: ReleaseEnvironment::Production,
                    status: DeploymentState::Succeeded,
                    revision: state.expected_revision.clone(),
                    external_run_id: None,
                    external_url: None,
                    detail: None,
                },
            ),
        )
        .unwrap_err();
        assert!(matches!(error, TransitionError::EnvironmentMismatch { .. }));
    }

    #[test]
    fn rollback_ignores_ci_but_still_requires_revision_and_health_evidence() {
        let mut state = state();
        state.environment = ReleaseEnvironment::Production;
        state.kind = ReleaseKind::Rollback;
        state.expected_revision = "c".repeat(40);
        let state = apply_event(
            &state,
            &callback(
                1,
                CallbackKind::Deployment {
                    environment: ReleaseEnvironment::Production,
                    status: DeploymentState::Succeeded,
                    revision: state.expected_revision.clone(),
                    external_run_id: None,
                    external_url: None,
                    detail: None,
                },
            ),
        )
        .unwrap();
        let state = apply_event(
            &state,
            &callback(
                2,
                CallbackKind::Verification {
                    environment: ReleaseEnvironment::Production,
                    status: VerificationState::Passed,
                    revision: state.expected_revision.clone(),
                    verifier: "rollback-probe".into(),
                    evidence_hash: "a".repeat(64),
                    detail: None,
                },
            ),
        )
        .unwrap();
        let state = apply_event(
            &state,
            &callback(
                3,
                CallbackKind::Observation {
                    environment: ReleaseEnvironment::Production,
                    status: ObservationState::Healthy,
                    revision: state.expected_revision.clone(),
                    evidence_hash: "b".repeat(64),
                    detail: None,
                },
            ),
        )
        .unwrap();
        assert_eq!(recommendation(&state).0, Recommendation::Ready);
    }
    #[test]
    fn failed_or_unknown_dispatch_blocks_release_recommendation() {
        let mut state = succeed(&state());
        assert_eq!(recommendation(&state).0, Recommendation::Ready);
        state.dispatch_state = DispatchState::Failed;
        assert_eq!(recommendation(&state).0, Recommendation::Blocked);
        state.dispatch_state = DispatchState::Unknown;
        let (recommendation, reasons) = recommendation(&state);
        assert_eq!(recommendation, Recommendation::Pending);
        assert!(
            reasons
                .iter()
                .any(|reason| reason.contains("dispatch is unknown")),
            "{reasons:?}"
        );
    }
}
