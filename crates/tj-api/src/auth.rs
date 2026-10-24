use argon2::password_hash::SaltString;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::handlers::AppState;

fn hash_password(password: &str) -> Result<String, String> {
    let salt = SaltString::encode_b64(uuid::Uuid::new_v4().as_bytes())
        .map_err(|e| format!("salt: {}", e))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| format!("hash: {}", e))
}

fn verify_password(password: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

pub fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let v = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let t = v.strip_prefix("Bearer ")?;
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

pub(crate) fn chat_session_id(user_id: &str, chat_id: &str) -> String {
    // An encoded tuple has no username/chat separator ambiguity.
    serde_json::to_string(&(user_id, chat_id)).expect("string tuple serialization")
}

#[cfg(test)]
mod scope_tests {
    use super::*;

    #[test]
    fn chat_ids_cannot_cross_user_boundaries() {
        assert_ne!(chat_session_id("a-b", "c"), chat_session_id("a", "b-c"));
        assert_ne!(
            chat_session_id("alice", "main"),
            chat_session_id("bob", "main")
        );
        assert_eq!(
            chat_session_id("alice", "main"),
            chat_session_id("alice", "main")
        );
    }
}

pub fn is_authorized(state: &AppState, headers: &HeaderMap) -> bool {
    match bearer_token(headers).map(|t| state.manager.user_for_token(&t)) {
        Some(Ok(Some(_))) => true,
        _ => false,
    }
}

pub fn require_auth(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<tj_db::User, axum::response::Response> {
    let token = bearer_token(headers).ok_or_else(|| {
        (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "authentication required" })),
        )
            .into_response()
    })?;
    match state.manager.user_for_token(&token) {
        Ok(Some(u)) => Ok(u),
        _ => Err((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "invalid or expired token" })),
        )
            .into_response()),
    }
}

fn user_json(u: &tj_db::User) -> serde_json::Value {
    serde_json::json!({
        "id": u.id,
        "username": u.username,
        "display_name": u.display_name,
        "is_admin": u.is_admin,
    })
}

#[derive(Debug, Deserialize)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

fn issue_session(
    state: &AppState,
    user: &tj_db::User,
) -> Result<serde_json::Value, axum::response::Response> {
    let token = format!("tjt_{}", uuid::Uuid::new_v4().simple());
    state.manager.issue_token(&token, &user.id).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response()
    })?;
    Ok(serde_json::json!({ "token": token, "user": user_json(user) }))
}

const DUMMY_HASH: &str =
    "$argon2id$v=19$m=19456,t=2,p=1$OXRoZWZha2VzYWx0aGVyZQ$RdescudvJCsgt3ub+b+dWRWJTmaaJObG";

pub fn validate_credentials(username: &str, password: &str) -> Result<(), String> {
    if username.trim().len() < 3 {
        return Err("username must be at least 3 characters".into());
    }
    if password.len() < 8 {
        return Err("password must be at least 8 characters".into());
    }
    Ok(())
}

pub async fn login(
    State(state): State<Arc<AppState>>,
    Json(req): Json<Credentials>,
) -> impl IntoResponse {
    let user = state.manager.get_user_by_username(&req.username);
    let user = match user {
        Ok(Some(u)) => u,
        _ => {
            let _ = verify_password(&req.password, DUMMY_HASH);
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({ "error": "invalid credentials" })),
            )
                .into_response();
        }
    };
    if !verify_password(&req.password, &user.password_hash) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "invalid credentials" })),
        )
            .into_response();
    }
    match issue_session(&state, &user) {
        Ok(v) => Json(v).into_response(),
        Err(r) => r,
    }
}

pub async fn register(
    State(state): State<Arc<AppState>>,
    Json(req): Json<Credentials>,
) -> impl IntoResponse {
    match state.manager.count_users() {
        Ok(0) => {}
        Ok(_) => return (StatusCode::FORBIDDEN, Json(serde_json::json!({ "error": "registration is closed — ask an admin to create your account" }))).into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": e.to_string() }))).into_response(),
    }
    if let Err(e) = validate_credentials(&req.username, &req.password) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e })),
        )
            .into_response();
    }
    let hash = match hash_password(&req.password) {
        Ok(h) => h,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e })),
            )
                .into_response();
        }
    };
    let user = tj_db::User {
        id: format!("user_{}", uuid::Uuid::new_v4()),
        username: req.username,
        password_hash: hash,
        display_name: None,
        is_admin: true,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    if let Err(e) = state.manager.db().bootstrap_admin(&user) {
        return (StatusCode::CONFLICT, Json(serde_json::json!({ "error": format!("registration lost the race or username taken: {}", e) }))).into_response();
    }
    match issue_session(&state, &user) {
        Ok(v) => Json(v).into_response(),
        Err(r) => r,
    }
}

pub async fn logout(State(state): State<Arc<AppState>>, headers: HeaderMap) -> impl IntoResponse {
    if let Some(token) = bearer_token(&headers) {
        if state.manager.revoke_token(&token).is_err() {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error":"Could not revoke session"})),
            )
                .into_response();
        }
    }
    Json(serde_json::json!({ "ok": true })).into_response()
}

pub async fn me(State(state): State<Arc<AppState>>, headers: HeaderMap) -> impl IntoResponse {
    match require_auth(&state, &headers) {
        Ok(u) => Json(user_json(&u)).into_response(),
        Err(r) => r,
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateUserRequest {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub is_admin: Option<bool>,
}

pub async fn list_users(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let actor = match require_auth(&state, &headers) {
        Ok(u) => u,
        Err(r) => return r,
    };
    if !actor.is_admin {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "admin required" })),
        )
            .into_response();
    }
    match state.manager.list_users() {
        Ok(users) => Json(serde_json::Value::Array(
            users.iter().map(user_json).collect(),
        ))
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub async fn create_user(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<CreateUserRequest>,
) -> impl IntoResponse {
    let actor = match require_auth(&state, &headers) {
        Ok(u) => u,
        Err(r) => return r,
    };
    if !actor.is_admin {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "admin required" })),
        )
            .into_response();
    }
    if let Err(e) = validate_credentials(&req.username, &req.password) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e })),
        )
            .into_response();
    }
    let hash = match hash_password(&req.password) {
        Ok(h) => h,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e })),
            )
                .into_response();
        }
    };
    let user = tj_db::User {
        id: format!("user_{}", uuid::Uuid::new_v4()),
        username: req.username,
        password_hash: hash,
        display_name: req.display_name,
        is_admin: req.is_admin.unwrap_or(false),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    if let Err(e) = state.manager.create_user(&user) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response();
    }
    Json(user_json(&user)).into_response()
}
