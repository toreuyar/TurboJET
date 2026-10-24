//! Durable diagnostic log read paths (TJ-024).
//!
//! These routes used to return empty placeholder arrays. They now read the
//! authorized, credential-redacted durable activity store. They are
//! administrator-only (enforced by `access::authorize`), run/project access is
//! still checked by the caller selecting a run, and retention is bounded by
//! `activity_retention_days`. They are diagnostics, not release evidence.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tj_db::StageLog;

use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct LogQuery {
    pub level: Option<String>,
    pub source: Option<String>,
    pub component: Option<String>,
    pub stage_id: Option<String>,
    pub flow_id: Option<String>,
    pub run_id: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub search: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct LogResponse {
    pub entries: Vec<serde_json::Value>,
    pub total_count: usize,
    pub filtered: bool,
    pub retention: String,
}

fn row_json(row: &StageLog) -> serde_json::Value {
    serde_json::json!({
        "id": row.id,
        "run_id": row.run_id.as_str(),
        "stage_id": row.stage_id,
        "stage_name": row.stage_name,
        "flow_name": row.flow_name,
        "kind": row.kind,
        "level": if matches!(row.kind.as_str(), "error" | "failed" | "output_validation") { "error" } else { "info" },
        "content": row.content,
        "created_at": row.created_at,
    })
}

fn apply(query: &LogQuery, rows: Vec<StageLog>) -> (Vec<serde_json::Value>, bool) {
    let filtered = query.level.is_some()
        || query.source.is_some()
        || query.component.is_some()
        || query.run_id.is_some()
        || query.flow_id.is_some()
        || query.since.is_some()
        || query.until.is_some()
        || query.search.is_some();
    let needle = query.search.as_deref().map(str::to_ascii_lowercase);
    let level = query.level.as_deref();
    let mut entries: Vec<_> = rows
        .into_iter()
        .filter(|row| {
            level.is_none_or(|level| {
                if level.eq_ignore_ascii_case("error") {
                    matches!(row.kind.as_str(), "error" | "failed" | "output_validation")
                } else {
                    true
                }
            })
        })
        .filter(|row| {
            query
                .run_id
                .as_deref()
                .is_none_or(|run| row.run_id.as_str() == run)
        })
        .filter(|row| {
            query
                .flow_id
                .as_deref()
                .is_none_or(|flow| row.flow_name.as_deref() == Some(flow))
        })
        .filter(|row| {
            query
                .since
                .as_deref()
                .is_none_or(|since| row.created_at.as_str() >= since)
        })
        .filter(|row| {
            query
                .until
                .as_deref()
                .is_none_or(|until| row.created_at.as_str() <= until)
        })
        .filter(|row| {
            needle
                .as_deref()
                .is_none_or(|needle| row.content.to_ascii_lowercase().contains(needle))
        })
        .map(|row| row_json(&row))
        .collect();
    let offset = query.offset.unwrap_or(0);
    if offset > 0 && offset < entries.len() {
        entries = entries.split_off(offset);
    }
    (entries, filtered)
}

fn retention(state: &AppState) -> String {
    format!(
        "{} days",
        state.config_loader.config().activity_retention_days
    )
}

pub async fn get_system_logs(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<LogQuery>,
) -> Result<Json<LogResponse>, StatusCode> {
    if !crate::auth::is_authorized(&state, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let limit = query.limit.unwrap_or(500).clamp(1, 2000) as i64;
    let rows = if let Some(run_id) = &query.run_id {
        state
            .manager
            .list_stage_logs(run_id, 0, 5000)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    } else {
        state
            .manager
            .db()
            .list_recent_stage_logs(5000)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    };
    let (entries, filtered) = apply(&query, rows.into_iter().take(limit as usize).collect());
    Ok(Json(LogResponse {
        total_count: entries.len(),
        entries,
        filtered,
        retention: retention(&state),
    }))
}

pub async fn get_container_logs(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(stage_id): Path<String>,
) -> Result<Json<LogResponse>, StatusCode> {
    if !crate::auth::is_authorized(&state, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let rows = state
        .manager
        .db()
        .list_stage_logs_for_stage(&stage_id, 2000)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let entries: Vec<_> = rows.iter().map(row_json).collect();
    Ok(Json(LogResponse {
        total_count: entries.len(),
        entries,
        filtered: false,
        retention: retention(&state),
    }))
}

pub async fn get_stage_logs(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(stage_id): Path<String>,
    Query(query): Query<LogQuery>,
) -> Result<Json<LogResponse>, StatusCode> {
    if !crate::auth::is_authorized(&state, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let rows = state
        .manager
        .db()
        .list_stage_logs_for_stage(&stage_id, 5000)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let (entries, filtered) = apply(&query, rows);
    Ok(Json(LogResponse {
        total_count: entries.len(),
        entries,
        filtered,
        retention: retention(&state),
    }))
}

pub async fn get_error_logs(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<LogQuery>,
) -> Result<Json<LogResponse>, StatusCode> {
    if !crate::auth::is_authorized(&state, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let rows = state
        .manager
        .db()
        .list_recent_stage_logs(5000)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut errors: Vec<_> = rows
        .into_iter()
        .filter(|row| matches!(row.kind.as_str(), "error" | "failed" | "output_validation"))
        .collect();
    errors.reverse();
    let (entries, _) = apply(&query, errors);
    Ok(Json(LogResponse {
        total_count: entries.len(),
        entries,
        filtered: true,
        retention: retention(&state),
    }))
}

#[derive(Debug, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(flatten)]
    pub filters: LogQuery,
}

#[derive(Debug, Serialize)]
pub struct SearchResponse {
    pub results: Vec<serde_json::Value>,
    pub total_matches: usize,
    pub query: String,
    pub retention: String,
}

pub async fn search_logs(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<SearchRequest>,
) -> Result<Json<SearchResponse>, StatusCode> {
    if !crate::auth::is_authorized(&state, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if req.query.trim().len() < 2 {
        return Err(StatusCode::BAD_REQUEST);
    }
    let rows = state
        .manager
        .db()
        .list_recent_stage_logs(5000)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut filters = req.filters;
    filters.search = Some(req.query.clone());
    let (results, _) = apply(&filters, rows);
    Ok(Json(SearchResponse {
        total_matches: results.len(),
        results,
        query: req.query,
        retention: retention(&state),
    }))
}
