//! TJ-039/TJ-040 delivery API: explicit release dispatch, immutable production
//! decisions, signed callbacks, polling reconciliation and read-only
//! provenance. External CI/CD retains execution authority; no handler merges,
//! releases or treats an unknown deployment as success.

use axum::{
    Json,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

use tj_config::system::DeliveryConfig;
use tj_db::delivery::{
    DeliveryEventRecord, DeliveryRelease, NewDeliveryAuthorization, NewDeliveryRelease,
};
use tj_db::publications::{PublicationState, RemotePublication};
use tj_delivery::{
    CALLBACK_EVENT_HEADER, CALLBACK_SIGNATURE_HEADER, CALLBACK_SOURCE_HEADER,
    CALLBACK_TIMESTAMP_HEADER, CallbackEvent, CallbackKind, DecisionAction, DeliveryAdapter,
    DispatchRequest, ForgejoActionsAdapter, ForgejoActionsConfig, ObservationState, Recommendation,
    ReleaseEnvironment, ReleaseIdentity, ReleaseKind, VerificationState,
    authorization_operation_key, release_operation_key, sign_callback, verify_callback,
};

use crate::auth::require_auth;
use crate::handlers::AppState;

pub struct DeliveryRuntime {
    config: Option<DeliveryConfig>,
    adapter: Option<Arc<dyn DeliveryAdapter>>,
    callback_secret: Option<Vec<u8>>,
    callback_window_secs: i64,
}

impl DeliveryRuntime {
    pub fn from_config(loader: &tj_config::ConfigLoader) -> anyhow::Result<Self> {
        let Some(config) = loader.config().delivery.clone() else {
            return Ok(Self {
                config: None,
                adapter: None,
                callback_secret: None,
                callback_window_secs: 0,
            });
        };
        anyhow::ensure!(
            config.provider == "forgejo_actions",
            "unsupported delivery provider '{}'; TJ-039 supports forgejo_actions",
            config.provider
        );
        anyhow::ensure!(
            config.callback_window_secs > 0 && config.callback_window_secs <= 86_400,
            "delivery callback_window_secs must be between 1 and 86400"
        );
        let token = std::env::var(&config.token_env).map_err(|_| {
            anyhow::anyhow!(
                "delivery token environment variable '{}' is not set",
                config.token_env
            )
        })?;
        let secret = std::env::var(&config.callback_secret_env).map_err(|_| {
            anyhow::anyhow!(
                "delivery callback secret environment variable '{}' is not set",
                config.callback_secret_env
            )
        })?;
        anyhow::ensure!(
            secret.len() >= 16,
            "delivery callback secret '{}' must contain at least 16 bytes",
            config.callback_secret_env
        );
        let adapter = ForgejoActionsAdapter::new(ForgejoActionsConfig {
            base_url: config.base_url.clone(),
            token,
            owner: config.owner.clone(),
            repo: config.repo.clone(),
            workflow: config.workflow.clone(),
            git_ref: config.git_ref.clone(),
            callback_base_url: config.callback_base_url.clone(),
        })?;
        let window = config.callback_window_secs;
        Ok(Self {
            config: Some(config),
            adapter: Some(Arc::new(adapter)),
            callback_secret: Some(secret.into_bytes()),
            callback_window_secs: window,
        })
    }

    fn integration_json(&self) -> serde_json::Value {
        match &self.config {
            Some(config) => json!({
                "configured": true,
                "provider": config.provider,
                "repository": format!("{}/{}", config.owner, config.repo),
                "workflow": config.workflow,
                "git_ref": config.git_ref,
                "callback_window_secs": config.callback_window_secs,
                "callback_path_template": "/v1/delivery/releases/{release_id}/callbacks",
                "execution_authority": "external_ci_cd",
                "limitation": "Local contract only until a real external workflow and deployment target produce live evidence.",
            }),
            None => json!({
                "configured": false,
                "provider": null,
                "execution_authority": "external_ci_cd",
                "limitation": "No delivery adapter configured. Delivery history remains inspectable; dispatch, polling and callbacks are disabled.",
            }),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct StartReleaseRequest {
    pub environment: ReleaseEnvironment,
    #[serde(default)]
    pub kind: Option<ReleaseKind>,
    #[serde(default)]
    pub publication_id: Option<String>,
    #[serde(default)]
    pub authorization_id: Option<String>,
    #[serde(default)]
    pub rollback_target_sha: Option<String>,
    #[serde(default)]
    pub source_release_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RetryDispatchRequest {
    #[serde(default)]
    pub acknowledge_unknown_external_effect: bool,
}

#[derive(Debug, Deserialize)]
pub struct DecisionRequest {
    #[serde(default)]
    pub decision: Option<DecisionAction>,
    #[serde(default)]
    pub kind: Option<ReleaseKind>,
    pub publication_id: String,
    #[serde(default)]
    pub staging_release_id: Option<String>,
    #[serde(default)]
    pub source_release_id: Option<String>,
    #[serde(default)]
    pub rollback_target_sha: Option<String>,
    pub idempotency_key: String,
    #[serde(default)]
    pub reason: Option<String>,
}

fn api_error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

fn access(state: &AppState, headers: &HeaderMap, run_id: &str) -> Result<tj_db::User, Response> {
    let user = require_auth(state, headers)?;
    if !state
        .manager
        .db()
        .can_access_run(&user.id, run_id)
        .unwrap_or(false)
    {
        return Err(api_error(StatusCode::FORBIDDEN, "Project access denied"));
    }
    Ok(user)
}

fn latest_publication(
    state: &AppState,
    run_id: &str,
    publication_id: Option<&str>,
) -> Result<RemotePublication, Response> {
    let publication = match publication_id {
        Some(id) => state.manager.db().publication(id).ok().flatten(),
        None => state
            .manager
            .db()
            .latest_publication_for_run(run_id)
            .ok()
            .flatten(),
    }
    .ok_or_else(|| {
        api_error(
            StatusCode::CONFLICT,
            "No remote publication exists for this run yet",
        )
    })?;
    if publication.run_id != run_id {
        return Err(api_error(
            StatusCode::CONFLICT,
            "Publication does not belong to this run",
        ));
    }
    if publication.push_state != PublicationState::Succeeded
        || publication.pr_state != PublicationState::Succeeded
    {
        return Err(api_error(
            StatusCode::CONFLICT,
            "Delivery requires a durable pushed branch and draft PR for the same exact publication",
        ));
    }
    let commit = publication
        .commit_sha
        .as_deref()
        .ok_or_else(|| api_error(StatusCode::CONFLICT, "Publication has no commit identity"))?;
    if publication.remote_sha.as_deref() != Some(commit) {
        return Err(api_error(
            StatusCode::CONFLICT,
            "Publication remote SHA does not match its commit identity",
        ));
    }
    Ok(publication)
}

fn release_identity<'a>(
    publication: &'a RemotePublication,
    project_id: &'a str,
    environment: ReleaseEnvironment,
    kind: ReleaseKind,
    expected_revision: &'a str,
    authorization_id: Option<&'a str>,
    source_release_id: Option<&'a str>,
) -> ReleaseIdentity<'a> {
    ReleaseIdentity {
        run_id: &publication.run_id,
        project_id,
        publication_id: &publication.id,
        selection_id: &publication.selection_id,
        task_id: &publication.task_id,
        plan_revision_id: &publication.plan_revision_id,
        plan_content_hash: &publication.plan_content_hash,
        packet_revision_id: &publication.packet_revision_id,
        packet_content_hash: &publication.packet_content_hash,
        pr_number: publication.pr_number,
        commit_sha: publication.commit_sha.as_deref().unwrap_or_default(),
        base_sha: publication.base_sha.as_deref().unwrap_or_default(),
        branch: &publication.branch,
        environment,
        kind,
        expected_revision,
        authorization_id,
        source_release_id,
    }
}

/// Immutable decisions are append-only, so the newest decision for the same
/// exact publication/kind/revision is authoritative. An earlier authorization
/// cannot be used after a later rejection.
fn authorization_is_current(
    state: &AppState,
    authorization: &tj_db::delivery::DeliveryAuthorization,
) -> Result<(), Response> {
    let decisions = state
        .manager
        .db()
        .delivery_authorizations_for_run(&authorization.run_id)
        .unwrap_or_default();
    let latest = decisions.iter().rev().find(|candidate| {
        candidate.publication_id == authorization.publication_id
            && candidate.kind == authorization.kind
            && candidate.expected_revision == authorization.expected_revision
    });
    if latest.map(|candidate| candidate.id.as_str()) != Some(authorization.id.as_str())
        || latest.is_none_or(|candidate| candidate.decision != DecisionAction::Authorize)
    {
        return Err(api_error(
            StatusCode::CONFLICT,
            "This production authorization is not the latest decision for the exact revision; a later decision supersedes it",
        ));
    }
    Ok(())
}

fn staging_ready(
    state: &AppState,
    publication: &RemotePublication,
    staging_release_id: &str,
) -> Result<DeliveryRelease, Response> {
    let release = state
        .manager
        .db()
        .delivery_release(staging_release_id)
        .ok()
        .flatten()
        .ok_or_else(|| api_error(StatusCode::CONFLICT, "Staging release was not found"))?;
    if release.run_id != publication.run_id
        || release.publication_id != publication.id
        || release.environment != ReleaseEnvironment::Staging
        || release.kind != ReleaseKind::Deploy
    {
        return Err(api_error(
            StatusCode::CONFLICT,
            "Staging release identity does not match this publication",
        ));
    }
    if release.expected_revision != publication.commit_sha.as_deref().unwrap_or_default() {
        return Err(api_error(
            StatusCode::CONFLICT,
            "Staging release revision does not match the exact publication commit",
        ));
    }
    if release.recommendation != Recommendation::Ready {
        return Err(api_error(
            StatusCode::CONFLICT,
            "Staging release is not ready: CI, deployment, independent verification and observation must all pass for the exact revision",
        ));
    }
    Ok(release)
}

fn accepted_event_id(
    events: &[DeliveryEventRecord],
    release_id: &str,
    kind: &str,
    predicate: impl Fn(&CallbackEvent) -> bool,
) -> Option<String> {
    events
        .iter()
        .rev()
        .filter(|event| {
            event.release_id == release_id && event.accepted && event.event_kind == kind
        })
        .find_map(|event| {
            serde_json::from_str::<CallbackEvent>(&event.payload_json)
                .ok()
                .filter(|event| predicate(event))
                .map(|_| event.event_id.clone())
        })
}

pub async fn bundle(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
) -> Response {
    if let Err(response) = access(&state, &headers, &run_id) {
        return response;
    }
    match state.manager.delivery_bundle(&run_id) {
        Ok(mut bundle) => {
            bundle["integration"] = state.delivery.integration_json();
            Json(bundle).into_response()
        }
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
    }
}

pub async fn create_decision(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
    Json(request): Json<DecisionRequest>,
) -> Response {
    let user = match access(&state, &headers, &run_id) {
        Ok(user) => user,
        Err(response) => return response,
    };
    if request.idempotency_key.trim().is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "idempotency_key is required");
    }
    let publication = match latest_publication(&state, &run_id, Some(&request.publication_id)) {
        Ok(publication) => publication,
        Err(response) => return response,
    };
    let decision = request.decision.unwrap_or(DecisionAction::Authorize);
    let kind = request.kind.unwrap_or(ReleaseKind::Deploy);
    let commit = publication.commit_sha.clone().unwrap_or_default();

    let (
        expected_revision,
        staging_release_id,
        staging_verification_event_id,
        staging_observation_event_id,
        source_release_id,
    ) = match (kind, decision) {
        (ReleaseKind::Deploy, DecisionAction::Authorize) => {
            if request
                .reason
                .as_deref()
                .map(str::trim)
                .is_none_or(str::is_empty)
            {
                return api_error(
                    StatusCode::BAD_REQUEST,
                    "production authorization requires an explicit reason",
                );
            }
            let Some(staging_id) = request.staging_release_id.as_deref() else {
                return api_error(
                    StatusCode::CONFLICT,
                    "Production deployment requires an exact verified staging release",
                );
            };
            let staging = match staging_ready(&state, &publication, staging_id) {
                Ok(staging) => staging,
                Err(response) => return response,
            };
            let events = state
                .manager
                .db()
                .delivery_events_for_release(&staging.id)
                .unwrap_or_default();
            let Some(verification_id) = accepted_event_id(
                &events,
                &staging.id,
                "verification",
                |event| matches!(&event.kind, CallbackKind::Verification { status: VerificationState::Passed, revision, .. } if *revision == commit),
            ) else {
                return api_error(
                    StatusCode::CONFLICT,
                    "Staging release has no accepted independent verification event for the exact commit",
                );
            };
            let Some(observation_id) = accepted_event_id(
                &events,
                &staging.id,
                "observation",
                |event| matches!(&event.kind, CallbackKind::Observation { status: ObservationState::Healthy, revision, .. } if *revision == commit),
            ) else {
                return api_error(
                    StatusCode::CONFLICT,
                    "Staging release has no accepted healthy observation event for the exact commit",
                );
            };
            (
                commit.clone(),
                Some(staging.id),
                Some(verification_id),
                Some(observation_id),
                None,
            )
        }
        (ReleaseKind::Deploy, DecisionAction::Reject) => (
            commit.clone(),
            request.staging_release_id.clone(),
            None,
            None,
            None,
        ),
        (ReleaseKind::Rollback, _) => {
            let Some(source_id) = request.source_release_id.as_deref() else {
                return api_error(
                    StatusCode::CONFLICT,
                    "Rollback authorization requires source_release_id",
                );
            };
            let Some(target) = request
                .rollback_target_sha
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
            else {
                return api_error(
                    StatusCode::CONFLICT,
                    "Rollback authorization requires an exact rollback_target_sha",
                );
            };
            let source = match state.manager.db().delivery_release(source_id) {
                Ok(Some(source)) => source,
                _ => {
                    return api_error(
                        StatusCode::CONFLICT,
                        "Rollback source release was not found",
                    );
                }
            };
            if source.run_id != run_id
                || source.publication_id != publication.id
                || source.environment != ReleaseEnvironment::Production
                || !matches!(
                    source.deployment_state,
                    tj_delivery::DeploymentState::Succeeded | tj_delivery::DeploymentState::Failed
                )
            {
                return api_error(
                    StatusCode::CONFLICT,
                    "Rollback source must be an existing production deployment (succeeded or failed) of this publication",
                );
            }
            if target == source.expected_revision {
                return api_error(
                    StatusCode::CONFLICT,
                    "Rollback target must differ from the source deployment revision",
                );
            }
            (target.to_string(), None, None, None, Some(source.id))
        }
    };

    let operation_key = authorization_operation_key(
        &run_id,
        &publication.id,
        decision,
        kind,
        &expected_revision,
        staging_release_id.as_deref(),
        source_release_id.as_deref(),
        &request.idempotency_key,
    );
    let now = chrono::Utc::now().to_rfc3339();
    let authorization = NewDeliveryAuthorization {
        id: format!("ddec_{}", uuid::Uuid::new_v4()),
        run_id: run_id.clone(),
        publication_id: publication.id.clone(),
        decision,
        environment: ReleaseEnvironment::Production,
        kind,
        commit_sha: commit,
        expected_revision,
        staging_release_id,
        staging_verification_event_id,
        staging_observation_event_id,
        source_release_id,
        task_id: publication.task_id.clone(),
        plan_revision_id: publication.plan_revision_id.clone(),
        plan_content_hash: publication.plan_content_hash.clone(),
        packet_revision_id: publication.packet_revision_id.clone(),
        packet_content_hash: publication.packet_content_hash.clone(),
        actor_id: user.id.clone(),
        actor_name: user
            .display_name
            .clone()
            .unwrap_or_else(|| user.username.clone()),
        reason: request.reason,
        idempotency_key: request.idempotency_key,
        operation_key,
        created_at: now,
    };
    match state
        .manager
        .db()
        .create_delivery_authorization(&authorization)
    {
        Ok((created, is_new)) => (
            if is_new {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            },
            Json(json!({ "decision": created, "replayed": !is_new })),
        )
            .into_response(),
        Err(error) => api_error(StatusCode::CONFLICT, error.to_string()),
    }
}

pub async fn start_release(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
    Json(request): Json<StartReleaseRequest>,
) -> Response {
    let user = match access(&state, &headers, &run_id) {
        Ok(user) => user,
        Err(response) => return response,
    };
    let Some(config) = state.delivery.config.as_ref() else {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Delivery integration is not configured; no external workflow was dispatched",
        );
    };
    let Some(adapter) = state.delivery.adapter.as_ref() else {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Delivery adapter is unavailable",
        );
    };
    let contract = match state.manager.run_contract(&run_id) {
        Ok(contract) => contract,
        Err(error) => {
            return api_error(
                StatusCode::CONFLICT,
                format!("Cannot verify the frozen delivery capability: {error:#}"),
            );
        }
    };
    let declared_delivery = contract
        .as_ref()
        .and_then(|contract| contract.external_delivery());
    if let Err(error) = tj_manager::capabilities::delivery::ensure_delivery_allowed(
        state.manager.db(),
        &run_id,
        declared_delivery,
    ) {
        return api_error(StatusCode::CONFLICT, format!("{error:#}"));
    }
    let publication = match latest_publication(&state, &run_id, request.publication_id.as_deref()) {
        Ok(publication) => publication,
        Err(response) => return response,
    };
    let Some(run) = state.manager.get_run(&run_id).ok().flatten() else {
        return api_error(StatusCode::NOT_FOUND, "Run not found");
    };
    let Some(project_id) = run.project_id().map(|id| id.0.clone()) else {
        return api_error(
            StatusCode::CONFLICT,
            "Delivery requires a project-scoped run",
        );
    };
    let kind = request.kind.unwrap_or(ReleaseKind::Deploy);
    let commit = publication.commit_sha.clone().unwrap_or_default();
    let (expected_revision, authorization_id, source_release_id) = match request.environment {
        ReleaseEnvironment::Staging => {
            if kind != ReleaseKind::Deploy {
                return api_error(
                    StatusCode::CONFLICT,
                    "Rollback is a production action and requires an exact authorization",
                );
            }
            (commit.clone(), None, None)
        }
        ReleaseEnvironment::Production => {
            let Some(auth_id) = request.authorization_id.as_deref() else {
                return api_error(
                    StatusCode::CONFLICT,
                    "Production action requires an immutable authorized decision for the exact revision",
                );
            };
            let authorization = match state.manager.db().delivery_authorization(auth_id) {
                Ok(Some(authorization)) => authorization,
                _ => {
                    return api_error(
                        StatusCode::CONFLICT,
                        "Production authorization was not found",
                    );
                }
            };
            if authorization.decision != DecisionAction::Authorize
                || authorization.run_id != run_id
                || authorization.publication_id != publication.id
                || authorization.environment != ReleaseEnvironment::Production
                || authorization.kind != kind
            {
                return api_error(
                    StatusCode::CONFLICT,
                    "Production authorization does not match this exact run/publication/kind",
                );
            }
            if let Err(response) = authorization_is_current(&state, &authorization) {
                return response;
            }
            if kind == ReleaseKind::Deploy {
                if authorization.expected_revision != commit {
                    return api_error(
                        StatusCode::CONFLICT,
                        "Production authorization does not bind the exact publication commit",
                    );
                }
                let Some(staging_id) = authorization.staging_release_id.as_deref() else {
                    return api_error(
                        StatusCode::CONFLICT,
                        "Production authorization has no staging release",
                    );
                };
                let staging = match staging_ready(&state, &publication, staging_id) {
                    Ok(staging) => staging,
                    Err(response) => return response,
                };
                if staging.id != staging_id {
                    return api_error(StatusCode::CONFLICT, "Staging identity changed");
                }
                (commit.clone(), Some(authorization.id), None)
            } else {
                let target = authorization.expected_revision.as_str();
                if request
                    .rollback_target_sha
                    .as_deref()
                    .is_some_and(|sha| sha != target)
                {
                    return api_error(
                        StatusCode::CONFLICT,
                        "Rollback target does not match its authorization",
                    );
                }
                if request.source_release_id.as_deref()
                    != authorization.source_release_id.as_deref()
                {
                    return api_error(
                        StatusCode::CONFLICT,
                        "Rollback source does not match its authorization",
                    );
                }
                (
                    authorization.expected_revision.clone(),
                    Some(authorization.id),
                    authorization.source_release_id.clone(),
                )
            }
        }
    };

    let identity = release_identity(
        &publication,
        &project_id,
        request.environment,
        kind,
        &expected_revision,
        authorization_id.as_deref(),
        source_release_id.as_deref(),
    );
    let operation_key = release_operation_key(&identity);
    let now = chrono::Utc::now().to_rfc3339();
    let new_release = NewDeliveryRelease {
        id: format!("drel_{}", uuid::Uuid::new_v4()),
        run_id: run_id.clone(),
        project_id,
        publication_id: publication.id.clone(),
        selection_id: publication.selection_id.clone(),
        task_id: publication.task_id.clone(),
        plan_revision_id: publication.plan_revision_id.clone(),
        plan_content_hash: publication.plan_content_hash.clone(),
        packet_revision_id: publication.packet_revision_id.clone(),
        packet_content_hash: publication.packet_content_hash.clone(),
        pr_number: publication.pr_number,
        pr_url: publication.pr_url.clone(),
        commit_sha: commit.clone(),
        base_sha: publication.base_sha.clone(),
        branch: publication.branch.clone(),
        environment: request.environment,
        kind,
        expected_revision: expected_revision.clone(),
        authorization_id: authorization_id.clone(),
        source_release_id: source_release_id.clone(),
        operation_key: operation_key.clone(),
        workflow: config.workflow.clone(),
        workflow_ref: config.git_ref.clone(),
        created_by: user.id.clone(),
        created_at: now,
    };
    let (release, created) = match state.manager.db().create_delivery_release(&new_release) {
        Ok(result) => result,
        Err(error) => return api_error(StatusCode::CONFLICT, error.to_string()),
    };
    if !created {
        return (
            StatusCode::OK,
            Json(json!({ "release": release, "replayed": true,
                "note": "Existing exact operation returned; no second external dispatch was made." })),
        ).into_response();
    }
    if let Err(error) = state.manager.db().begin_delivery_dispatch(&release.id) {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string());
    }
    let callback_path = format!("/v1/delivery/releases/{}/callbacks", release.id);
    let receipt = adapter
        .dispatch(&DispatchRequest {
            release_id: release.id.clone(),
            run_id: run_id.clone(),
            environment: request.environment,
            kind,
            commit_sha: commit.clone(),
            expected_revision: expected_revision.clone(),
            operation_key,
            callback_path,
        })
        .await;
    match receipt {
        Ok(receipt) => {
            if let Err(error) = state.manager.db().complete_delivery_dispatch(
                &release.id,
                receipt.external_run_id.as_deref(),
                receipt.external_url.as_deref(),
            ) {
                return api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string());
            }
            match state.manager.db().delivery_release(&release.id) {
                Ok(Some(release)) => (
                    StatusCode::CREATED,
                    Json(json!({ "release": release, "dispatched": true,
                        "execution_authority": "external_ci_cd" })),
                )
                    .into_response(),
                _ => api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Persisted delivery release disappeared",
                ),
            }
        }
        Err(failure) => {
            let state_update = if failure.uncertain {
                state
                    .manager
                    .db()
                    .mark_delivery_dispatch_unknown(&release.id, &failure.message)
            } else {
                state
                    .manager
                    .db()
                    .fail_delivery_dispatch(&release.id, &failure.message)
            };
            if let Err(error) = state_update {
                tracing::error!(release_id = %release.id, %error, "Cannot persist delivery dispatch failure");
            }
            let release = state
                .manager
                .db()
                .delivery_release(&release.id)
                .ok()
                .flatten();
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "error": failure.message,
                    "uncertain": failure.uncertain,
                    "release": release,
                    "note": if failure.uncertain {
                        "External workflow may have started. State is durably unknown; reconcile by signed callback or polling. No retry is automatic."
                    } else {
                        "External workflow refused the dispatch; no success is claimed."
                    }
                })),
            ).into_response()
        }
    }
}

pub async fn retry_dispatch(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((run_id, release_id)): Path<(String, String)>,
    Json(request): Json<RetryDispatchRequest>,
) -> Response {
    if let Err(response) = access(&state, &headers, &run_id) {
        return response;
    }
    if !request.acknowledge_unknown_external_effect {
        return api_error(
            StatusCode::CONFLICT,
            "Re-dispatch requires acknowledgement that an unknown attempt may have reached the external system",
        );
    }
    if state.delivery.config.is_none() {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Delivery integration is not configured",
        );
    }
    let Some(adapter) = state.delivery.adapter.as_ref() else {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Delivery adapter is unavailable",
        );
    };
    let release = match state.manager.db().delivery_release(&release_id) {
        Ok(Some(release)) if release.run_id == run_id => release,
        _ => {
            return api_error(
                StatusCode::NOT_FOUND,
                "Delivery release not found for this run",
            );
        }
    };
    // Revalidate the immutable production authorization and staging evidence
    // before re-dispatching. A failed dispatch never bypasses the release gate.
    if release.environment == ReleaseEnvironment::Production {
        let Some(auth_id) = release.authorization_id.as_deref() else {
            return api_error(
                StatusCode::CONFLICT,
                "Production release has no authorization identity",
            );
        };
        let authorization = match state.manager.db().delivery_authorization(auth_id) {
            Ok(Some(authorization)) => authorization,
            _ => {
                return api_error(
                    StatusCode::CONFLICT,
                    "Production authorization no longer exists",
                );
            }
        };
        if authorization.decision != DecisionAction::Authorize
            || authorization.run_id != run_id
            || authorization.kind != release.kind
            || authorization.expected_revision != release.expected_revision
        {
            return api_error(
                StatusCode::CONFLICT,
                "Production authorization no longer matches this release",
            );
        }
        if let Err(response) = authorization_is_current(&state, &authorization) {
            return response;
        }
        if release.kind == ReleaseKind::Deploy {
            let publication =
                match latest_publication(&state, &run_id, Some(&release.publication_id)) {
                    Ok(publication) => publication,
                    Err(response) => return response,
                };
            let Some(staging_id) = authorization.staging_release_id.as_deref() else {
                return api_error(
                    StatusCode::CONFLICT,
                    "Production authorization has no staging release",
                );
            };
            if let Err(response) = staging_ready(&state, &publication, staging_id) {
                return response;
            }
        }
    }
    if let Err(error) = state.manager.db().prepare_delivery_redispatch(&release.id) {
        return api_error(StatusCode::CONFLICT, error.to_string());
    }
    let callback_path = format!("/v1/delivery/releases/{}/callbacks", release.id);
    let receipt = adapter
        .dispatch(&DispatchRequest {
            release_id: release.id.clone(),
            run_id: run_id.clone(),
            environment: release.environment,
            kind: release.kind,
            commit_sha: release.commit_sha.clone(),
            expected_revision: release.expected_revision.clone(),
            operation_key: release.operation_key.clone(),
            callback_path,
        })
        .await;
    match receipt {
        Ok(receipt) => {
            if let Err(error) = state.manager.db().complete_delivery_dispatch(
                &release.id,
                receipt.external_run_id.as_deref(),
                receipt.external_url.as_deref(),
            ) {
                return api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string());
            }
            match state.manager.db().delivery_release(&release.id) {
                Ok(Some(release)) => (
                    StatusCode::OK,
                    Json(json!({
                        "release": release,
                        "dispatched": true,
                        "replayed": false,
                        "execution_authority": "external_ci_cd",
                        "note": "Re-dispatch reused the immutable operation key; the external workflow must deduplicate it.",
                    })),
                ).into_response(),
                _ => api_error(StatusCode::INTERNAL_SERVER_ERROR, "Persisted delivery release disappeared"),
            }
        }
        Err(failure) => {
            let update = if failure.uncertain {
                state
                    .manager
                    .db()
                    .mark_delivery_dispatch_unknown(&release.id, &failure.message)
            } else {
                state
                    .manager
                    .db()
                    .fail_delivery_dispatch(&release.id, &failure.message)
            };
            if let Err(error) = update {
                tracing::error!(release_id = %release.id, %error, "Cannot persist delivery redispatch failure");
            }
            let release = state
                .manager
                .db()
                .delivery_release(&release.id)
                .ok()
                .flatten();
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": failure.message, "uncertain": failure.uncertain, "release": release })),
            ).into_response()
        }
    }
}

pub async fn reconcile_release(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((run_id, release_id)): Path<(String, String)>,
) -> Response {
    if let Err(response) = access(&state, &headers, &run_id) {
        return response;
    }
    let Some(adapter) = state.delivery.adapter.as_ref() else {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Delivery integration is not configured",
        );
    };
    let release = match state.manager.db().delivery_release(&release_id) {
        Ok(Some(release)) if release.run_id == run_id => release,
        _ => {
            return api_error(
                StatusCode::NOT_FOUND,
                "Delivery release not found for this run",
            );
        }
    };
    if release.kind == ReleaseKind::Rollback {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "Rollback has no CI poll path; signed deployment and health-verification callbacks are required",
                "release": release,
            })),
        ).into_response();
    }
    let runs = match adapter.poll(&release.commit_sha).await {
        Ok(runs) => runs,
        Err(failure) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": failure.message, "uncertain": failure.uncertain, "release": release })),
            ).into_response();
        }
    };
    // Polling must identify the same durable external run that dispatch
    // recorded. A commit can have unrelated CI runs; taking the first match
    // could promote an unrelated workflow to this release's CI evidence.
    let Some(expected_external_run) = release.external_run_id.as_deref() else {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "No durable external run id was recorded for this release; polling cannot safely identify its workflow. Use a signed callback or reconcile the dispatch response first.",
                "release": release,
            })),
        ).into_response();
    };
    let Some(observation) = runs.into_iter().find(|observation| {
        observation.external_run_id == expected_external_run
            && observation.commit_sha.as_deref() == Some(release.commit_sha.as_str())
    }) else {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "error": format!("No external run '{}' matched the exact commit '{}'; no success is inferred.", expected_external_run, release.commit_sha),
                "release": release,
            })),
        ).into_response();
    };
    let event = CallbackEvent {
        event_id: format!(
            "poll:{}:{}",
            observation.external_run_id,
            observation.status.as_str()
        ),
        sequence: release.last_sequence + 1,
        source: "forgejo_actions_poll".into(),
        // Stable payload for the same external run/status so a repeated poll is
        // an idempotent replay, not an event-id/payload conflict.
        occurred_at: None,
        kind: CallbackKind::Ci {
            status: observation.status,
            revision: release.commit_sha.clone(),
            external_run_id: observation.external_run_id.clone(),
            external_url: observation.external_url.clone(),
            workflow: observation.workflow.clone(),
            detail: Some(
                "reconciled by polling; CI evidence only, deployment still requires its own proof"
                    .into(),
            ),
        },
    };
    let payload = serde_json::to_string(&event).unwrap_or_default();
    match state.manager.db().apply_delivery_callback(
        &release_id,
        &event,
        "forgejo_actions_poll",
        &payload,
    ) {
        Ok(application) => (
            StatusCode::OK,
            Json(json!({
                "reconciled": application.accepted,
                "replayed": application.replayed,
                "error": application.error,
                "release": application.release,
                "event": application.event,
            })),
        )
            .into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
    }
}

/// External callback endpoint. Authentication is HMAC over timestamp, event id
/// and body; this route intentionally does not accept user bearer tokens.
pub async fn callback(
    State(state): State<Arc<AppState>>,
    Path(release_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(secret) = state.delivery.callback_secret.as_deref() else {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Delivery callbacks are not configured",
        );
    };
    let header = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
    let Some(timestamp) = header(CALLBACK_TIMESTAMP_HEADER) else {
        return api_error(
            StatusCode::UNAUTHORIZED,
            "Missing delivery callback timestamp",
        );
    };
    let Some(event_id) = header(CALLBACK_EVENT_HEADER) else {
        return api_error(
            StatusCode::UNAUTHORIZED,
            "Missing delivery callback event id",
        );
    };
    let Some(signature) = header(CALLBACK_SIGNATURE_HEADER) else {
        return api_error(
            StatusCode::UNAUTHORIZED,
            "Missing delivery callback signature",
        );
    };
    if let Err(auth_error) = verify_callback(
        secret,
        timestamp,
        event_id,
        signature,
        &body,
        chrono::Utc::now(),
        state.delivery.callback_window_secs,
    ) {
        return api_error(StatusCode::UNAUTHORIZED, auth_error.to_string());
    }
    let mut event: CallbackEvent = match serde_json::from_slice(&body) {
        Ok(event) => event,
        Err(error) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                format!("Invalid callback JSON: {error}"),
            );
        }
    };
    if event.event_id != event_id {
        return api_error(
            StatusCode::CONFLICT,
            "Signed event id does not match the callback body event_id",
        );
    }
    if let Some(source) = header(CALLBACK_SOURCE_HEADER) {
        if source.is_empty() {
            return api_error(StatusCode::UNAUTHORIZED, "Empty delivery callback source");
        }
        if !event.source.is_empty() && event.source != source {
            return api_error(
                StatusCode::CONFLICT,
                "Callback source header and body disagree",
            );
        }
        event.source = source.to_string();
    }
    if event.source.trim().is_empty() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "Callback source identity is required",
        );
    }
    let payload = String::from_utf8_lossy(&body).to_string();
    match state
        .manager
        .db()
        .apply_delivery_callback(&release_id, &event, &event.source, &payload)
    {
        Ok(application) => {
            let status = if !application.accepted {
                StatusCode::CONFLICT
            } else if application.replayed {
                StatusCode::OK
            } else {
                StatusCode::ACCEPTED
            };
            (
                status,
                Json(json!({
                    "accepted": application.accepted,
                    "replayed": application.replayed,
                    "error": application.error,
                    "release_id": release_id,
                    "event_id": application.event.event_id,
                    "recommendation": application.release.recommendation,
                    "last_sequence": application.release.last_sequence,
                })),
            )
                .into_response()
        }
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
    }
}

/// Deterministic local drill helper. Not mounted on the router: tests and the
/// operator drill use this to sign exactly the same canonical body the external
/// workflow would send.
pub fn sign_drill_callback(
    secret: &[u8],
    event_id: &str,
    event: &CallbackEvent,
) -> (String, String) {
    let timestamp = chrono::Utc::now().to_rfc3339();
    let body = serde_json::to_string(event).unwrap_or_default();
    (
        timestamp.clone(),
        sign_callback(secret, &timestamp, event_id, body.as_bytes()),
    )
}
