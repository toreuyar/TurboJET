//! First-run readiness and authorized diagnostics/support export (TJ-023/024).
//!
//! `/api/ready` is intentionally public and exposes only dependency names,
//! booleans and actionable configuration labels. Run activity, model/tool
//! transcripts and exports require authentication and project access.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path as FsPath;
use std::sync::Arc;

use crate::auth::require_auth;
use crate::handlers::AppState;

#[derive(Debug, Deserialize, Default)]
pub struct ActivityQuery {
    pub after: Option<i64>,
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize, Default)]
pub struct PruneQuery {
    pub before: Option<String>,
}

fn default_agent_provider(state: &AppState) -> Option<(String, String, String, String)> {
    let sys = state.config_loader.config();
    let agent = sys
        .agents
        .iter()
        .find(|agent| agent.default && agent.enabled)
        .or_else(|| sys.agents.iter().find(|agent| agent.enabled))?;
    let provider = sys
        .providers
        .iter()
        .find(|provider| provider.id == agent.provider_id)?;
    let model = if agent.model.is_empty() {
        provider.default_model.clone()
    } else {
        agent.model.clone()
    };
    Some((
        agent.id.clone(),
        provider.id.clone(),
        model,
        provider.api_key_env.clone(),
    ))
}

fn workspace_check(state: &AppState) -> (bool, String) {
    let path = std::path::PathBuf::from(state.manager.workspace_base());
    match std::fs::create_dir_all(&path) {
        Ok(()) => {
            let probe = path.join(format!(".tj-readiness-{}", uuid::Uuid::new_v4()));
            match std::fs::write(&probe, b"ok") {
                Ok(()) => {
                    let _ = std::fs::remove_file(probe);
                    (true, path.display().to_string())
                }
                Err(error) => (false, format!("{} ({error})", path.display())),
            }
        }
        Err(error) => (false, format!("{} ({error})", path.display())),
    }
}

fn ui_check() -> (bool, Option<String>) {
    let configured = std::env::var("TURBOJET_WEBUI_PATH").ok();
    let candidate = configured
        .as_deref()
        .filter(|p| !p.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            let path = std::path::PathBuf::from("webui/dist/index.html");
            path.is_file().then_some(path)
        });
    match candidate {
        Some(path) => (path.is_file(), Some(path.display().to_string())),
        None => (false, None),
    }
}

pub async fn readiness_public(State(state): State<Arc<AppState>>) -> Response {
    let db_check = state.manager.db().schema_version();
    let db_error = db_check.as_ref().err().map(|error| error.to_string());
    let projects = state.manager.db().list_projects();
    let users = state.manager.count_users();
    let (workspace_ok, workspace_detail) = workspace_check(&state);
    let (ui_ok, ui_detail) = ui_check();
    let provider = default_agent_provider(&state);
    let provider_ready = provider.as_ref().is_some_and(|(_, _, _, env)| {
        std::env::var(env)
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
    });
    let secrets_ready = state.secrets_store().is_some();
    let isolation = tj_tools::isolation::LinuxSandbox::status();
    let schema_version = db_check.as_ref().ok().copied();
    let admin_exists = users.as_ref().map(|n| *n > 0).unwrap_or(false);
    let project_count = projects.as_ref().map(|p| p.len()).unwrap_or(0);
    let workflow_count = state
        .manager
        .list_workflows()
        .map(|workflows| workflows.len())
        .unwrap_or(0);
    let workflows_ok = workflow_count > 0;
    let blocked = db_check.is_err() || !workspace_ok || !provider_ready;
    let degraded = !secrets_ready || !ui_ok || !isolation.available || !workflows_ok;
    let status = if blocked {
        "blocked"
    } else if degraded {
        "degraded"
    } else {
        "ready"
    };

    let checks = vec![
        json!({
            "name": "database", "ok": db_check.is_ok(), "schema_version": schema_version,
            "detail": db_error,
        }),
        json!({"name": "workspace", "ok": workspace_ok, "detail": workspace_detail}),
        json!({
            "name": "provider",
            "ok": provider_ready,
            "provider_id": provider.as_ref().map(|p| p.1.clone()),
            "model": provider.as_ref().map(|p| p.2.clone()),
            "agent_id": provider.as_ref().map(|p| p.0.clone()),
            "credential_env": provider.as_ref().map(|p| p.3.clone()),
            "detail": if provider_ready { "API key is present in the process environment" }
                      else { "Set the named provider API-key environment variable, then restart TurboJet" },
        }),
        json!({
            "name": "secrets", "ok": secrets_ready,
            "detail": if secrets_ready { "TURBOJET_MASTER_KEY is valid; project secrets can be opened" }
                      else { "Set TURBOJET_MASTER_KEY to 64 hex characters for encrypted project secrets" },
        }),
        json!({"name": "webui", "ok": ui_ok, "path": ui_detail,
            "detail": if ui_ok { "Built UI is present" } else { "Set TURBOJET_WEBUI_PATH to the built index.html" }}),
        json!({
            "name": "workflows",
            "ok": workflows_ok,
            "count": workflow_count,
            "detail": if workflows_ok {
                format!("{workflow_count} immutable workflow version(s) registered")
            } else {
                "No workflow installed; configure a workflow source or import a workflow.".to_string()
            }
        }),
        json!({
            "name": "execution_isolation",
            "ok": isolation.available,
            "detail": if isolation.available {
                format!("{} ABI {}: model shell commands are confined and fail closed", isolation.backend, isolation.abi)
            } else {
                format!("BLOCKED: model shell commands fail closed ({})", isolation.reason.unwrap_or_else(|| "isolation unavailable".into()))
            }
        }),
    ];
    Json(json!({
        "status": status,
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_ms": state.startup.elapsed().as_millis(),
        "checks": checks,
        "first_run": {
            "admin_exists": admin_exists,
            "project_count": project_count,
            "setup_required": !admin_exists || project_count == 0,
            "next_step": if !admin_exists { "Create the first administrator account" }
                else if !workflows_ok { "No workflow installed; configure a workflow source or import a workflow." }
                else if project_count == 0 { "Create a project from a registered workflow" }
                else { "Start a run or ask Chief" },
        },
        "provider": {
            "agent_id": provider.as_ref().map(|p| p.0.clone()),
            "provider_id": provider.as_ref().map(|p| p.1.clone()),
            "model": provider.as_ref().map(|p| p.2.clone()),
            "credential_env": provider.as_ref().map(|p| p.3.clone()),
            "ready": provider_ready,
        },
        "activity_retention_days": state.config_loader.config().activity_retention_days,
    }))
    .into_response()
}

pub async fn readiness_authenticated(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = require_auth(&state, &headers) {
        return response;
    }
    readiness_public(State(state)).await
}

/// Durable activity resync. `after` is the last row id the client has seen;
/// a reconnect fetches only newer rows and reports whether more remain.
pub async fn activity(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
    Query(query): Query<ActivityQuery>,
) -> Response {
    if let Err(response) = require_auth(&state, &headers) {
        return response;
    }
    let after = query.after.unwrap_or(0).max(0);
    let limit = query.limit.unwrap_or(500).clamp(1, 2000);
    let rows = match state.manager.list_stage_logs(&run_id, after, limit + 1) {
        Ok(rows) => rows,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": error.to_string()})),
            )
                .into_response();
        }
    };
    let has_more = rows.len() as i64 > limit;
    let rows: Vec<_> = rows.into_iter().take(limit as usize).collect();
    let next_after = rows.iter().map(|row| row.id).max().unwrap_or(after);
    let attempts = state
        .manager
        .db()
        .list_provider_activity(&run_id, after, limit)
        .unwrap_or_default();
    Json(json!({
        "run_id": run_id,
        "activity": rows,
        "provider_attempts": attempts,
        "next_after": next_after,
        "has_more": has_more,
        "resync": true,
    }))
    .into_response()
}

pub async fn diagnostics(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
) -> Response {
    let user = match require_auth(&state, &headers) {
        Ok(user) => user,
        Err(response) => return response,
    };
    if !state
        .manager
        .db()
        .can_access_run(&user.id, &run_id)
        .unwrap_or(false)
    {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"Project access denied"})),
        )
            .into_response();
    }
    let run = match state.manager.get_run(&run_id) {
        Ok(Some(run)) => run,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error":"Run not found"})),
            )
                .into_response();
        }
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":error.to_string()})),
            )
                .into_response();
        }
    };
    let flows = state
        .manager
        .db()
        .load_flows_for_run(&run_id)
        .unwrap_or_default();
    let events = state.manager.list_events(&run_id).unwrap_or_default();
    let mut verdicts: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for event in events
        .iter()
        .filter(|event| event.kind == "stage_completed")
    {
        if let (Some(stage), Some(payload)) = (&event.stage_name, &event.payload)
            && let Ok(value) = serde_json::from_str::<Value>(payload)
            && let Some(verdict) = value.get("verdict").and_then(Value::as_str)
        {
            verdicts.insert(stage.clone(), verdict.to_string());
        }
    }
    let mut stages = Vec::new();
    for flow in &flows {
        for stage in state
            .manager
            .db()
            .load_stages_for_flow(flow.id())
            .unwrap_or_default()
        {
            stages.push(json!({
                "flow_id": flow.id(), "flow_name": flow.flow_name(),
                "stage_id": stage.id(), "stage_name": stage.stage_name(),
                "status": stage.status(), "attempt_iteration": stage.iteration(),
                "verdict": verdicts.get(stage.stage_name()),
                "error": stage.error(),
            }));
        }
    }
    let recent_events: Vec<_> = events.iter().rev().take(30).cloned().collect();
    let decisions = state
        .manager
        .db()
        .list_gate_decisions(&run_id)
        .unwrap_or_default();
    let corrections = state
        .manager
        .list_stage_logs(&run_id, 0, 5000)
        .unwrap_or_default();
    let correction_counts = json!({
        "llm_calls": corrections.iter().filter(|row| row.kind == "llm_request").count(),
        "output_corrections": corrections.iter().filter(|row| row.kind == "output_validation" && row.content != "ok").count(),
        "silent_turn_nudges": corrections.iter().filter(|row| row.kind == "nudge").count(),
    });
    let usage = state
        .manager
        .db()
        .provider_usage_summary(&run_id)
        .unwrap_or_default();
    let attempts = state
        .manager
        .db()
        .provider_attempt_summary(&run_id)
        .unwrap_or_default();
    let budget = state.manager.budget(&run_id).ok().flatten();
    let recovery = state.manager.recovery(&run_id).ok().flatten();
    let publications = state.manager.publications(&run_id).unwrap_or_default();
    Json(json!({
        "run_id": run_id,
        "status": run.status(),
        "state_version": run.state_version(),
        "gate_instance_id": run.gate_instance_id(),
        "gate_name": run.gate_name(),
        "generated_at": Utc::now().to_rfc3339(),
        "retention_days": state.config_loader.config().activity_retention_days,
        "stages": stages,
        "routes": decisions.iter().map(|decision| json!({
            "gate_name": decision.gate_name,
            "approved": decision.approved,
            "disposition": decision.disposition,
            "packet_revision_id": decision.packet_revision_id,
            "plan_revision_id": decision.plan_revision_id,
            "plan_content_hash": decision.plan_content_hash,
            "packet_content_hash": decision.packet_content_hash,
            "state_version": decision.state_version,
            "created_at": decision.created_at,
        })).collect::<Vec<_>>(),
        "corrections": correction_counts,
        "provider_usage": usage,
        "provider_attempts": attempts,
        "budget": budget,
        "recovery": recovery,
        "publications": publications,
        "recent_events": recent_events,
    }))
    .into_response()
}

pub async fn support_export(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
) -> Response {
    let user = match require_auth(&state, &headers) {
        Ok(user) => user,
        Err(response) => return response,
    };
    if !state
        .manager
        .db()
        .can_access_run(&user.id, &run_id)
        .unwrap_or(false)
    {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"Project access denied"})),
        )
            .into_response();
    }
    let run = match state.manager.get_run(&run_id) {
        Ok(Some(run)) => run,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error":"Run not found"})),
            )
                .into_response();
        }
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":error.to_string()})),
            )
                .into_response();
        }
    };
    let events = state.manager.list_events(&run_id).unwrap_or_default();
    let activity = state
        .manager
        .list_stage_logs(&run_id, 0, 100_000)
        .unwrap_or_default();
    let packet = state.manager.db().packet_bundle(&run_id).ok();
    let budget = state.manager.budget(&run_id).ok().flatten();
    let usage = state
        .manager
        .db()
        .provider_usage_summary(&run_id)
        .unwrap_or_default();
    let attempts = state
        .manager
        .db()
        .list_provider_activity(&run_id, 0, 100_000)
        .unwrap_or_default();
    let decisions = state
        .manager
        .db()
        .list_gate_decisions(&run_id)
        .unwrap_or_default();
    let publications = state.manager.publications(&run_id).unwrap_or_default();
    let delivery = state.manager.delivery_bundle(&run_id).unwrap_or_else(|_| {
        json!({
            "run_id": run_id, "releases": [], "decisions": [], "callbacks": [],
        })
    });
    let payload = json!({
        "format_version": 1,
        "generated_at": Utc::now().to_rfc3339(),
        "run": run,
        "events": events,
        "activity": activity,
        "provider_attempts": attempts,
        "provider_usage": usage,
        "budget": budget,
        "gate_decisions": decisions,
        "packet": packet,
        "publications": publications,
        "delivery": delivery,
        "redaction": "Credentials, bearer tokens, cookies and private-key blocks are redacted at write and export boundaries.",
        "retention_days": state.config_loader.config().activity_retention_days,
    });
    let text = tj_db::redaction::redact_sensitive(
        &serde_json::to_string_pretty(&payload).unwrap_or_default(),
    );
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            ),
            (
                header::CONTENT_DISPOSITION,
                HeaderValue::from_str(&format!(
                    "attachment; filename=\"turbojet-support-{run_id}.json\""
                ))
                .unwrap(),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        text,
    )
        .into_response()
}

/// Admin-only bounded retention cleanup. This never deletes runs, artifacts,
/// packet revisions, gate decisions or project secrets.
pub async fn prune_activity(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(query): Json<PruneQuery>,
) -> Response {
    let user = match require_auth(&state, &headers) {
        Ok(user) => user,
        Err(response) => return response,
    };
    if !state.manager.db().is_admin_user(&user.id).unwrap_or(false) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"Administrator access required"})),
        )
            .into_response();
    }
    let before = match query.before {
        Some(before) => before,
        None => {
            let days = state.config_loader.config().activity_retention_days;
            match chrono::Duration::try_days(i64::try_from(days).unwrap_or(i64::MAX)) {
                Some(duration) => (Utc::now() - duration).to_rfc3339(),
                None => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error":"invalid retention setting"})),
                    )
                        .into_response();
                }
            }
        }
    };
    match state.manager.db().prune_activity_before(&before) {
        Ok((logs, attempts)) => Json(json!({"before": before, "stage_logs_deleted": logs, "provider_attempts_deleted": attempts})).into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error":error.to_string()}))).into_response(),
    }
}

pub fn is_existing_path(path: &str) -> bool {
    FsPath::new(path).exists()
}
