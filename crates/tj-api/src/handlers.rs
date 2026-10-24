use dashmap::DashMap;
use std::sync::Arc;
use tj_agent::{AgentResponse, AgentSession};
use tj_db::PipelineNotification;
use tj_manager::{ManagerEvent, ManagerHandle, SpawnResult};
use tj_secrets::SecretsStore;
use tokio::sync::broadcast;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::IntoResponse,
    response::sse::{Event, Sse},
    routing::{delete, get, post, put},
};
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::ReceiverStream;
use tower_http::cors::CorsLayer;

use crate::auth::require_auth;
use crate::log_handlers::{
    get_container_logs, get_error_logs, get_stage_logs, get_system_logs, search_logs,
};
use crate::logging::log_http;
use crate::secrets::*;

pub struct AppState {
    pub manager: ManagerHandle,
    pub sessions: DashMap<String, AgentSession>,
    pub pipeline_events: broadcast::Sender<PipelineNotification>,
    pub secrets_store: Option<Arc<dyn SecretsStore>>,
    pub config_loader: Arc<tj_config::ConfigLoader>,
    pub delivery: crate::delivery::DeliveryRuntime,
    pub startup: std::time::Instant,
}

impl AppState {
    pub fn secrets_store(&self) -> Option<Arc<dyn SecretsStore>> {
        self.secrets_store.clone()
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct CreateRunRequest {
    description: String,
    project_id: String,
    lane_id: String,
    #[serde(default)]
    repo_url: Option<String>,
    #[serde(default)]
    base_branch: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ChatCompletionRequest {
    #[allow(dead_code)]
    model: Option<String>,
    messages: Vec<ChatMessage>,
    stream: Option<bool>,
    #[serde(default)]
    tools: Option<Vec<ClientToolDef>>,
    /// Separates concurrent conversations ("chats") per user.
    #[serde(default)]
    chat_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
enum MessageContent {
    Text(String),
    Parts(Vec<serde_json::Value>),
}

impl MessageContent {
    fn to_text(&self) -> Option<String> {
        match self {
            MessageContent::Text(s) => Some(s.clone()),
            MessageContent::Parts(parts) => {
                let texts: Vec<String> = parts
                    .iter()
                    .filter_map(|p| {
                        if p.get("type").and_then(|t| t.as_str()) == Some("text") {
                            p.get("text")
                                .and_then(|t| t.as_str())
                                .map(|s| s.to_string())
                        } else {
                            None
                        }
                    })
                    .collect();
                if texts.is_empty() {
                    None
                } else {
                    Some(texts.join("\n"))
                }
            }
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    #[serde(default)]
    content: Option<MessageContent>,
    #[serde(default)]
    tool_calls: Option<Vec<ClientToolCall>>,
    #[serde(default)]
    tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClientToolDef {
    #[serde(rename = "type")]
    tool_type: Option<String>,
    function: Option<ClientToolFunction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClientToolFunction {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default, rename = "parameters")]
    input_schema: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClientToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: Option<String>,
    function: ClientToolCallFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClientToolCallFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Serialize)]
struct ChatCompletionResponse {
    id: String,
    object: String,
    created: u64,
    model: String,
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Serialize)]
struct ChatChoice {
    index: u32,
    message: ChatMessageResponse,
    finish_reason: String,
}

#[derive(Debug, Serialize)]
struct ChatMessageResponse {
    role: String,
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GateDecisionRequest {
    approved: bool,
    gate_instance_id: String,
    expected_state_version: i64,
    idempotency_key: String,
    #[serde(default)]
    reason: Option<String>,
    /// Exact packet revision/hash under review (TJ-021). Required whenever the
    /// active gate is bound to a requirements packet.
    #[serde(default)]
    packet_revision_id: Option<String>,
    #[serde(default)]
    packet_content_hash: Option<String>,
    /// Exact structured plan revision/hash under review (TJ-032). Required
    /// whenever the active gate is plan-bound.
    #[serde(default)]
    plan_revision_id: Option<String>,
    #[serde(default)]
    plan_content_hash: Option<String>,
    /// Exact generic output artifact under review, when the active gate is
    /// artifact-bound (generic repository-free workflows).
    #[serde(default)]
    output_artifact_id: Option<String>,
    #[serde(default)]
    output_content_hash: Option<String>,
    #[serde(default)]
    output_stage_name: Option<String>,
    /// Explicit acknowledgement when approving a packet with honest unknowns.
    #[serde(default)]
    acknowledge_incomplete: bool,
}

#[derive(Debug, Serialize)]
struct ChatCompletionChunk {
    id: String,
    object: String,
    created: u64,
    model: String,
    choices: Vec<ChunkChoice>,
}

#[derive(Debug, Serialize)]
struct ChunkChoice {
    index: u32,
    delta: serde_json::Value,
    finish_reason: Option<String>,
}

pub async fn build_app(spawn_result: SpawnResult) -> Result<Router, anyhow::Error> {
    let model_name = spawn_result.handle.agent.model_name().to_string();
    let pipeline_tx = spawn_result.handle.pipeline_events.clone();
    let delivery = crate::delivery::DeliveryRuntime::from_config(&spawn_result.config_loader)?;

    let app_state = Arc::new(AppState {
        manager: spawn_result.handle,
        sessions: DashMap::new(),
        pipeline_events: pipeline_tx.clone(),
        secrets_store: spawn_result.secrets_store,
        config_loader: spawn_result.config_loader,
        delivery,
        startup: std::time::Instant::now(),
    });

    {
        let state = app_state.clone();
        let mut rx = pipeline_tx.subscribe();
        tokio::spawn(async move {
            while let Ok(event) = rx.recv().await {
                let run_id = event.run_id().to_string();
                let mut matching = vec![];
                for entry in state.sessions.iter() {
                    if entry.value().job_id.as_deref() == Some(&run_id) {
                        matching.push(entry.key().clone());
                    }
                }
                for session_id in matching {
                    if let Some(mut session) = state.sessions.get_mut(&session_id) {
                        let agent_event = match &event {
                            PipelineNotification::FlowCompleted {
                                run_id,
                                flow,
                                summary,
                            } => tj_agent::PipelineEvent::FlowCompleted {
                                run_id: run_id.clone(),
                                flow: flow.clone(),
                                summary: summary.clone(),
                            },
                            PipelineNotification::GateReached {
                                run_id,
                                flow,
                                summary,
                            } => tj_agent::PipelineEvent::GateReached {
                                run_id: run_id.clone(),
                                flow: flow.clone(),
                                summary: summary.clone(),
                            },
                            PipelineNotification::StageCompleted {
                                run_id,
                                stage,
                                summary,
                            } => tj_agent::PipelineEvent::StageCompleted {
                                run_id: run_id.clone(),
                                stage: stage.clone(),
                                summary: summary.clone(),
                            },
                            PipelineNotification::StageWaitingForInput {
                                run_id,
                                stage,
                                message,
                            } => tj_agent::PipelineEvent::StageWaitingForInput {
                                run_id: run_id.clone(),
                                stage: stage.clone(),
                                message: message.clone(),
                            },
                            PipelineNotification::PipelineCompleted { run_id, summary } => {
                                tj_agent::PipelineEvent::PipelineCompleted {
                                    run_id: run_id.clone(),
                                    summary: summary.clone(),
                                }
                            }
                            PipelineNotification::PipelineFailed {
                                run_id,
                                reason,
                                user_message,
                            } => tj_agent::PipelineEvent::PipelineFailed {
                                run_id: run_id.clone(),
                                reason: reason.clone(),
                                user_message: user_message.clone(),
                            },
                        };
                        state.manager.agent.inject_event(&mut session, &agent_event);
                    }
                }
            }
        });
    }

    let model_name_clone = model_name.clone();
    let app = Router::new()
        .route("/", get(root_webui))
        .route("/api", get(root_descriptor))
        .route("/api/health", get(health_check))
        .route("/api/ready", get(crate::diagnostics::readiness_public))
        .route(
            "/v1/readiness",
            get(crate::diagnostics::readiness_authenticated),
        )
        .route(
            "/v1/admin/activity/prune",
            post(crate::diagnostics::prune_activity),
        )
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/models", get(list_models))
        .route("/v1/pipeline/runs", get(list_runs).post(create_run))
        .route("/v1/pipeline/runs/{id}", get(get_run))
        .route("/v1/pipeline/runs/{id}/tree", get(get_run_tree))
        .route("/v1/projects", get(list_projects).post(create_project))
        .route("/v1/auth/login", post(crate::auth::login))
        .route("/v1/auth/register", post(crate::auth::register))
        .route("/v1/auth/logout", post(crate::auth::logout))
        .route("/v1/me", get(crate::auth::me))
        .route(
            "/v1/users",
            get(crate::auth::list_users).post(crate::auth::create_user),
        )
        .route("/v1/chats/{chat_id}", delete(delete_chat))
        .route("/v1/pipeline/runs/{id}/decisions", get(get_run_decisions))
        .route("/v1/pipeline/runs/{id}/events", get(get_run_events))
        .route(
            "/v1/pipeline/runs/{id}/stage-logs",
            get(get_stage_log_stream_data),
        )
        .route(
            "/v1/pipeline/runs/{id}/input",
            post(post_run_input).get(get_run_input),
        )
        .route("/v1/pipeline/runs/{id}/questions", get(get_run_questions))
        .route("/v1/pipeline/runs/{id}/bundle", get(get_run_bundle))
        .route("/v1/pipeline/runs/{id}/packet", get(get_run_packet))
        .route(
            "/v1/pipeline/runs/{id}/packet/revisions",
            post(save_packet_revision),
        )
        .route(
            "/v1/pipeline/runs/{id}/packet/export",
            get(export_run_packet),
        )
        .route("/v1/pipeline/runs/{id}/plan", get(get_run_plan))
        .route(
            "/v1/pipeline/runs/{id}/plan/revisions",
            post(save_plan_revision),
        )
        .route("/v1/pipeline/runs/{id}/plan/export", get(export_run_plan))
        .route("/v1/pipeline/runs/{id}/plan/tasks", get(get_plan_tasks))
        .route(
            "/v1/pipeline/runs/{id}/plan/select-task",
            post(select_plan_task),
        )
        .route("/v1/pipeline/runs/{id}/budget", get(get_run_budget))
        .route(
            "/v1/pipeline/runs/{id}/activity",
            get(crate::diagnostics::activity),
        )
        .route(
            "/v1/pipeline/runs/{id}/diagnostics",
            get(crate::diagnostics::diagnostics),
        )
        .route(
            "/v1/pipeline/runs/{id}/support-export",
            get(crate::diagnostics::support_export),
        )
        .route("/v1/pipeline/runs/{id}/recovery", get(get_run_recovery))
        .route(
            "/v1/pipeline/runs/{id}/publication",
            get(get_run_publication),
        )
        .route(
            "/v1/pipeline/runs/{id}/delivery",
            get(crate::delivery::bundle),
        )
        .route(
            "/v1/pipeline/runs/{id}/delivery/releases",
            post(crate::delivery::start_release),
        )
        .route(
            "/v1/pipeline/runs/{id}/delivery/decisions",
            post(crate::delivery::create_decision),
        )
        .route(
            "/v1/pipeline/runs/{id}/delivery/releases/{release_id}/reconcile",
            post(crate::delivery::reconcile_release),
        )
        .route(
            "/v1/pipeline/runs/{id}/delivery/releases/{release_id}/retry-dispatch",
            post(crate::delivery::retry_dispatch),
        )
        .route(
            "/v1/delivery/releases/{release_id}/callbacks",
            post(crate::delivery::callback),
        )
        .route("/v1/pipeline/runs/{id}/retry", post(retry_run))
        .route("/v1/pipeline/runs/{id}/files", get(list_run_files))
        .route("/v1/pipeline/runs/{id}/files/{*path}", get(read_run_file))
        .route("/v1/pipeline/runs/{id}", delete(delete_run))
        .route(
            "/v1/pipeline/runs/{id}/stage-logs/stream",
            get(sse_stage_logs),
        )
        .route("/v1/pipeline/runs/{id}/input", delete(cancel_run_input))
        .route("/v1/pipeline/runs/{id}/artifacts", get(get_run_artifacts))
        .route(
            "/v1/pipeline/artifacts/{artifact_id}/fork",
            post(fork_artifact),
        )
        .route("/v1/workflows", get(get_workflows))
        .route("/v1/workflows/{id}", get(get_workflow_detail))
        .route("/v1/workflows/import", post(import_workflows))
        .route("/v1/profiles", get(get_profiles))
        .route("/v1/pipeline/runs/{id}/output", get(get_run_output))
        .route("/v1/lanes", get(list_lanes))
        .route("/v1/lanes/{id}/workflow", put(pin_lane_workflow))
        .route("/v1/pipeline/runs/{id}/approve", post(approve_gate))
        .route("/v1/pipeline/runs/{id}/cancel", post(cancel_run))
        .route(
            "/v1/pipeline/runs/{id}/merge-complete",
            post(merge_complete),
        )
        .route("/v1/pipeline/events", get(sse_pipeline_events))
        // Log reading endpoints
        .route("/api/logs/system", get(get_system_logs))
        .route("/api/logs/containers/{stage_id}", get(get_container_logs))
        .route("/api/logs/stages/{stage_id}", get(get_stage_logs))
        .route("/api/logs/errors", get(get_error_logs))
        .route("/api/logs/search", post(search_logs))
        // Secrets management endpoints
        .route(
            "/v1/projects/{project_id}/secrets",
            post(set_secret).get(list_secrets),
        )
        .route(
            "/v1/projects/{project_id}/secrets/{key}",
            get(get_secret).delete(delete_secret),
        )
        // SSH key management endpoints
        .route(
            "/v1/projects/{project_id}/ssh-key",
            post(generate_ssh_key)
                .get(get_ssh_key)
                .delete(delete_ssh_key),
        )
        .route(
            "/v1/projects/{project_id}/ssh-known-hosts",
            put(set_ssh_known_hosts)
                .get(get_ssh_known_hosts)
                .delete(delete_ssh_known_hosts),
        )
        .route(
            "/v1/projects/{project_id}/provision",
            post(reprovision_project),
        )
        .route(
            "/v1/projects/{project_id}/members",
            get(crate::access::members),
        )
        .route(
            "/v1/projects/{project_id}/members/{user_id}",
            put(crate::access::membership).delete(crate::access::membership),
        )
        .route_layer(axum::middleware::from_fn_with_state(
            app_state.manager.db().clone(),
            crate::access::authorize,
        ))
        .layer(CorsLayer::new())
        .layer(axum::middleware::from_fn(log_http))
        .with_state(app_state);

    tracing::info!(model = %model_name_clone, "Default agent loaded");
    Ok(app)
}

type EventStream = std::pin::Pin<
    Box<dyn tokio_stream::Stream<Item = Result<Event, std::convert::Infallible>> + Send>,
>;

pub(crate) fn sse_unauthorized() -> axum::response::Response {
    let stream = async_stream::stream! {
        yield Ok(Event::default().event("auth").data("authentication required: pass ?token=<token>"));
    };
    Sse::new(Box::pin(stream) as EventStream).into_response()
}

fn make_chunk_id() -> String {
    format!("chatcmpl-{}", uuid::Uuid::new_v4())
}

fn make_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

async fn chat_completions(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<ChatCompletionRequest>,
) -> impl IntoResponse {
    let actor = match require_auth(&state, &headers) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let chat = req.chat_id.clone().unwrap_or_else(|| "main".into());
    let session_id = crate::auth::chat_session_id(&actor.id, &chat);
    let is_stream = req.stream.unwrap_or(false);
    let model_name = state.manager.agent.model_name().to_string();

    {
        let mut session = state
            .sessions
            .entry(session_id.clone())
            .or_insert_with(|| state.manager.agent.create_session(session_id.clone()));

        if let Some(msg) = req.messages.last()
            && msg.role == "tool"
            && let (Some(call_id), Some(content)) = (&msg.tool_call_id, &msg.content)
        {
            state.manager.agent.inject_client_tool_result(
                &mut session,
                call_id,
                content.to_text().unwrap_or_default(),
            );
        }
    }

    let mut session = state
        .sessions
        .get(&session_id)
        .map(|r| r.value().clone())
        .unwrap_or_else(|| state.manager.agent.create_session(session_id.clone()));

    let user_msg = req
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .and_then(|m| m.content.as_ref().and_then(|c| c.to_text()))
        .unwrap_or_default();

    let client_tools: Vec<tj_provider::types::ToolDef> = req
        .tools
        .unwrap_or_default()
        .into_iter()
        .filter(|t| {
            if let Some(ref func) = t.function {
                !state.manager.agent.tool_names().contains(&func.name)
            } else {
                false
            }
        })
        .filter_map(|t| {
            let func = t.function?;
            Some(tj_provider::types::ToolDef {
                tool_type: "function".into(),
                function: tj_provider::types::ToolFunction {
                    name: func.name,
                    description: func.description.unwrap_or_default(),
                    parameters: func
                        .input_schema
                        .unwrap_or(serde_json::json!({"type": "object", "properties": {}})),
                },
            })
        })
        .collect();

    let operations = state
        .manager
        .operations_for_token(crate::auth::bearer_token(&headers).unwrap_or_default());
    let response = match state
        .manager
        .agent
        .handle_message(&mut session, &user_msg, client_tools, &operations)
        .await
    {
        Ok(resp) => resp,
        Err(e) => {
            tracing::error!(error = %e, "Agent error");
            if let Some(mut entry) = state.sessions.get_mut(&session_id) {
                entry.value_mut().clone_from(&session);
            }
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };

    if let Some(mut entry) = state.sessions.get_mut(&session_id) {
        entry.value_mut().clone_from(&session);
    }

    match &response {
        AgentResponse::ToolCalls { calls } => {
            let tool_calls_json: Vec<serde_json::Value> = calls
                .iter()
                .map(|tc| {
                    serde_json::json!({
                        "id": tc.id,
                        "type": "function",
                        "function": {
                            "name": tc.name,
                            "arguments": tc.arguments.to_string()
                        }
                    })
                })
                .collect();

            let resp = ChatCompletionResponse {
                id: make_chunk_id(),
                object: "chat.completion".into(),
                created: make_timestamp(),
                model: model_name,
                choices: vec![ChatChoice {
                    index: 0,
                    message: ChatMessageResponse {
                        role: "assistant".into(),
                        content: None,
                        tool_calls: Some(tool_calls_json),
                    },
                    finish_reason: "tool_calls".into(),
                }],
            };
            (StatusCode::OK, Json(resp)).into_response()
        }
        AgentResponse::Text { content } => {
            if is_stream {
                let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, axum::Error>>(16);

                let chunk_id = make_chunk_id();
                let chunk_id_stop = chunk_id.clone();
                let model_name_stream = model_name.clone();

                let role_event = Event::default().data(
                    serde_json::to_string(&ChatCompletionChunk {
                        id: chunk_id.clone(),
                        object: "chat.completion.chunk".into(),
                        created: make_timestamp(),
                        model: model_name.clone(),
                        choices: vec![ChunkChoice {
                            index: 0,
                            delta: serde_json::json!({"role": "assistant"}),
                            finish_reason: None,
                        }],
                    })
                    .unwrap(),
                );
                let _ = tx.send(Ok(role_event)).await;

                let content_clone = content.clone();
                let chunk_id_content = chunk_id.clone();
                tokio::spawn(async move {
                    let chunk = ChatCompletionChunk {
                        id: chunk_id_content,
                        object: "chat.completion.chunk".into(),
                        created: make_timestamp(),
                        model: model_name_stream.clone(),
                        choices: vec![ChunkChoice {
                            index: 0,
                            delta: serde_json::json!({"content": content_clone}),
                            finish_reason: None,
                        }],
                    };
                    let _ = tx
                        .send(Ok(
                            Event::default().data(serde_json::to_string(&chunk).unwrap())
                        ))
                        .await;

                    let done_chunk = ChatCompletionChunk {
                        id: chunk_id_stop,
                        object: "chat.completion.chunk".into(),
                        created: make_timestamp(),
                        model: model_name_stream,
                        choices: vec![ChunkChoice {
                            index: 0,
                            delta: serde_json::json!({}),
                            finish_reason: Some("stop".into()),
                        }],
                    };
                    let _ = tx
                        .send(Ok(
                            Event::default().data(serde_json::to_string(&done_chunk).unwrap())
                        ))
                        .await;
                    let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
                });

                let stream = ReceiverStream::new(rx);
                let sse = Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default());

                return (
                    StatusCode::OK,
                    [(
                        header::CONTENT_TYPE,
                        HeaderValue::from_static("text/event-stream"),
                    )],
                    sse,
                )
                    .into_response();
            }

            let resp = ChatCompletionResponse {
                id: make_chunk_id(),
                object: "chat.completion".into(),
                created: make_timestamp(),
                model: model_name,
                choices: vec![ChatChoice {
                    index: 0,
                    message: ChatMessageResponse {
                        role: "assistant".into(),
                        content: Some(content.clone()),
                        tool_calls: None,
                    },
                    finish_reason: "stop".into(),
                }],
            };
            (StatusCode::OK, Json(resp)).into_response()
        }
    }
}

async fn list_models(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(serde_json::json!({
        "object": "list",
        "data": [{ "id": state.manager.agent.model_name(), "object": "model", "owned_by": "turbojet" }]
    }))
}

async fn create_run(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<CreateRunRequest>,
) -> impl IntoResponse {
    let actor = match require_auth(&state, &headers) {
        Ok(u) => u,
        Err(r) => return r,
    };
    if !state
        .manager
        .db()
        .can_access_project(&actor.id, &req.project_id)
        .unwrap_or(false)
    {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error":"access denied"})),
        )
            .into_response();
    }
    if let Err(e) = state
        .manager
        .validate_run_target(&req.project_id, &req.lane_id)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e })),
        )
            .into_response();
    }
    match state.manager.create_run(
        &req.description,
        Some(&req.project_id),
        Some(&req.lane_id),
        req.repo_url.as_deref(),
        req.base_branch.as_deref(),
        Some(&actor.username),
    ) {
        Ok(run) => {
            tracing::info!(run_id = %run.id(), "Job created");

            {
                let mut session = state
                    .sessions
                    .entry("default".into())
                    .or_insert_with(|| state.manager.agent.create_session("default".into()));
                session.job_id = Some(run.id().to_string());
            }

            (StatusCode::OK, Json(serde_json::to_value(&run).unwrap())).into_response()
        }
        Err(e) => {
            let message = e.to_string();
            let status = if message.contains("predates explicit")
                || message.contains("no pinned workflow")
                || message.contains("pins missing workflow")
            {
                StatusCode::CONFLICT
            } else {
                StatusCode::BAD_REQUEST
            };
            (status, Json(serde_json::json!({ "error": message }))).into_response()
        }
    }
}

async fn health_check() -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "healthy",
        "service": "TurboJet v2",
        "timestamp": chrono::Utc::now().to_rfc3339()
    }))
}

fn api_descriptor_json() -> serde_json::Value {
    serde_json::json!({
        "service": "TurboJet v2",
        "status": "ok",
        "api": {
            "health": "/api/health",
            "ready": "/api/ready",
            "readiness": "/v1/readiness",
            "runs": "/v1/pipeline/runs",
            "run_tree": "/v1/pipeline/runs/{id}/tree",
            "run_events": "/v1/pipeline/runs/{id}/events",
            "run_artifacts": "/v1/pipeline/runs/{id}/artifacts",
            "fork_artifact": "/v1/pipeline/artifacts/{artifact_id}/fork",
            "run_decisions": "/v1/pipeline/runs/{id}/decisions",
            "run_budget": "/v1/pipeline/runs/{id}/budget",
            "run_activity": "/v1/pipeline/runs/{id}/activity",
            "run_diagnostics": "/v1/pipeline/runs/{id}/diagnostics",
            "run_support_export": "/v1/pipeline/runs/{id}/support-export",
            "run_recovery": "/v1/pipeline/runs/{id}/recovery",
            "run_retry": "/v1/pipeline/runs/{id}/retry",
            "projects": "/v1/projects",
            "workflows": "/v1/workflows",
            "events_sse": "/v1/pipeline/events",
            "chat": "/v1/chat/completions"
        }
    })
}

async fn root_descriptor() -> impl IntoResponse {
    Json(api_descriptor_json())
}

async fn root_webui() -> axum::response::Response {
    let path = std::env::var("TURBOJET_WEBUI_PATH").unwrap_or_default();
    if !path.is_empty() {
        if let Ok(html) = tokio::fs::read_to_string(&path).await {
            return (
                [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
                axum::response::Html(html),
            )
                .into_response();
        }
    }
    Json(api_descriptor_json()).into_response()
}

async fn list_runs(State(state): State<Arc<AppState>>, headers: HeaderMap) -> impl IntoResponse {
    let user = match require_auth(&state, &headers) {
        Ok(user) => user,
        Err(r) => return r,
    };
    match state.manager.list_runs() {
        Ok(runs) => Json(
            runs.into_iter()
                .filter(|run| {
                    state
                        .manager
                        .db()
                        .can_access_run(&user.id, run.id())
                        .unwrap_or(false)
                })
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn get_run(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = require_auth(&state, &headers) {
        return r;
    }
    match state.manager.get_run(&id) {
        Ok(Some(run)) => {
            let mut value = serde_json::to_value(&run).unwrap_or_else(|_| serde_json::json!({}));
            if let Some(object) = value.as_object_mut() {
                object.insert(
                    "gate_binding".into(),
                    normalized_gate_binding(state.manager.db(), run.gate_instance_id())
                        .unwrap_or(serde_json::Value::Null),
                );
            }
            Json(value).into_response()
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "Job not found" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn approve_gate(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<GateDecisionRequest>,
) -> impl IntoResponse {
    let actor = match require_auth(&state, &headers) {
        Ok(u) => u,
        Err(r) => return r,
    };
    match state.manager.db().can_access_run(&actor.id, &id) {
        Ok(true) => {}
        Ok(false) => {
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({ "error": "Project access denied" })),
            )
                .into_response();
        }
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response();
        }
    }
    // An incomplete packet may be approved only with an explicit
    // acknowledgement. The DB binding check still decides whether the exact
    // revision/hash is current, so a concurrent revision conflicts below.
    if req.approved {
        match state
            .manager
            .db()
            .gate_packet_binding(&req.gate_instance_id)
        {
            Ok(Some(binding))
                if binding.completeness == "incomplete" && !req.acknowledge_incomplete =>
            {
                return (StatusCode::CONFLICT, Json(serde_json::json!({
                    "error": "This packet revision is incomplete; confirm the open unknowns/decisions before approving it"
                }))).into_response();
            }
            Ok(Some(binding)) if binding.completeness == "contradictory" => {
                return (StatusCode::CONFLICT, Json(serde_json::json!({
                    "error": "This packet revision contains unresolved contradictions; request changes before approval"
                }))).into_response();
            }
            Err(error) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": error.to_string() })),
                )
                    .into_response();
            }
            _ => {}
        }
    }
    let decision = tj_db::GateDecision {
        id: String::new(),
        run_id: tj_db::RunId(id.clone()),
        gate_name: String::new(),
        actor_id: actor.id.clone(),
        actor_name: actor.display_name.clone().unwrap_or(actor.username.clone()),
        approved: req.approved,
        reason: req.reason.clone(),
        note: None,
        created_at: chrono::Utc::now().to_rfc3339(),
        gate_instance_id: Some(req.gate_instance_id),
        idempotency_key: Some(req.idempotency_key),
        expected_state_version: Some(req.expected_state_version),
        state_version: None,
        disposition: None,
        packet_revision_id: req.packet_revision_id,
        packet_content_hash: req.packet_content_hash,
        plan_revision_id: req.plan_revision_id,
        plan_content_hash: req.plan_content_hash,
        output_artifact_id: req.output_artifact_id,
        output_content_hash: req.output_content_hash,
        output_stage_name: req.output_stage_name,
        acknowledge_incomplete: req.acknowledge_incomplete,
    };
    match state
        .manager
        .submit_gate_decision(decision, req.expected_state_version)
        .await
    {
        Ok(receipt) => Json(receipt).into_response(),
        Err(error) => {
            let message = error.to_string();
            let lower = message.to_lowercase();
            let status = if lower.contains("conflict") || message.contains("Idempotency key") {
                StatusCode::CONFLICT
            } else if lower.contains("not found") {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::BAD_REQUEST
            };
            (status, Json(serde_json::json!({ "error": message }))).into_response()
        }
    }
}

async fn list_projects(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let user = match require_auth(&state, &headers) {
        Ok(user) => user,
        Err(r) => return r,
    };
    let projects = match state.manager.list_projects() {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };
    let mut out = Vec::with_capacity(projects.len());
    for p in projects {
        if !state
            .manager
            .db()
            .can_access_project(&user.id, &p.id)
            .unwrap_or(false)
        {
            continue;
        }
        let lanes = state
            .manager
            .list_lanes_for_project(&p.id)
            .unwrap_or_default()
            .into_iter()
            .map(|l| {
                serde_json::json!({
                    "id": l.id,
                    "name": l.name,
                    "smg_path": l.smg_path,
                    "workflow_id": l.workflow_id,
                })
            })
            .collect::<Vec<_>>();
        out.push(serde_json::json!({
            "id": p.id,
            "name": p.name,
            "repo_url": p.repo_url,
            "base_branch": p.base_branch,
            "workspace_path": p.workspace_path,
            "profile": p.profile,
            "catalog_id": p.catalog_id,
            "workspace_kind": p.workspace_kind,
            "created_at": p.created_at,
            "lanes": lanes,
        }));
    }
    Json(serde_json::Value::Array(out)).into_response()
}

async fn get_run_events(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = require_auth(&state, &headers) {
        return r;
    }
    match state.manager.list_events(&id) {
        Ok(list) => Json(serde_json::to_value(&list).unwrap_or_default()).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn get_stage_log_stream_data(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    if let Err(r) = require_auth(&state, &headers) {
        return r;
    }
    let after: i64 = q.get("after").and_then(|v| v.parse().ok()).unwrap_or(0);
    let limit: i64 = q
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(500)
        .min(2000);
    match state.manager.list_stage_logs(&id, after, limit) {
        Ok(list) => Json(serde_json::to_value(&list).unwrap_or_default()).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Deserialize, Default)]
struct RetryRunBody {
    /// Required when the interruption may have left external effects behind.
    #[serde(default)]
    acknowledge_unknown_effects: bool,
}

#[derive(Deserialize)]
struct RunInputBody {
    text: String,
    #[serde(default)]
    stage_id: Option<String>,
    /// Durable question identity (TJ-014). Required for structured answers.
    #[serde(default)]
    question_id: Option<String>,
    #[serde(default)]
    attempt: Option<u32>,
    #[serde(default)]
    idempotency_key: Option<String>,
}

async fn post_run_input(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<RunInputBody>,
) -> impl IntoResponse {
    if let Err(r) = require_auth(&state, &headers) {
        return r;
    }
    if body.text.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "text is required" })),
        )
            .into_response();
    }
    match state.manager.send_web_input(
        &id,
        &body.text,
        body.stage_id.as_deref(),
        body.question_id.as_deref(),
        body.attempt,
        body.idempotency_key.as_deref(),
    ) {
        Ok(receipt) => (
            StatusCode::ACCEPTED,
            Json(serde_json::to_value(receipt).unwrap_or_default()),
        )
            .into_response(),
        Err(e) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": e })),
        )
            .into_response(),
    }
}

async fn get_run_questions(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    if let Err(r) = require_auth(&state, &headers) {
        return r;
    }
    let open_only = q
        .get("status")
        .map(|status| status == "open")
        .unwrap_or(true);
    match state.manager.list_questions(&id, open_only) {
        Ok(questions) => {
            let items = questions.into_iter().map(|question| serde_json::json!({
                "id": question.id,
                "run_id": question.run_id.as_str(),
                "flow_id": question.flow_id,
                "stage_id": question.stage_id,
                "stage_name": question.stage_name,
                "flow_name": question.flow_name,
                "attempt": question.attempt,
                "status": question.status,
                "request": serde_json::from_str::<serde_json::Value>(&question.request).ok(),
                "answer": question.answer.as_deref()
                    .and_then(|answer| serde_json::from_str::<serde_json::Value>(answer).ok()),
                "answer_source": question.answer_source,
                "created_at": question.created_at,
                "answered_at": question.answered_at,
                "closed_at": question.closed_at,
                "close_reason": question.close_reason,
            })).collect::<Vec<_>>();
            Json(serde_json::json!({ "questions": items })).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn get_run_bundle(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = require_auth(&state, &headers) {
        return r;
    }
    match state.manager.execution_bundle_record(&id) {
        Ok(Some(record)) => {
            let bundle = serde_json::from_str::<serde_json::Value>(&record.bundle_json).ok();
            let schema_version = bundle
                .as_ref()
                .and_then(|value| value.get("schema_version"))
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let capability_hash = bundle
                .as_ref()
                .and_then(|value| value.get("capability_hash"))
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let capability = bundle
                .as_ref()
                .and_then(|value| value.get("capability_contract"))
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            Json(serde_json::json!({
                "id": record.id,
                "run_id": record.run_id.as_str(),
                "workflow_id": record.workflow_id,
                "bundle_hash": record.bundle_hash,
                "legacy": record.legacy,
                "reproducible": !record.legacy,
                "created_at": record.created_at,
                "schema_version": schema_version,
                "capability_hash": capability_hash,
                "capability": capability,
                "bundle": bundle,
            }))
            .into_response()
        }
        Ok(None) => Json(serde_json::json!({
            "run_id": id,
            "legacy": true,
            "reproducible": false,
            "error": "legacy run admitted before execution bundles; its original files were not frozen",
        })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": e.to_string() }))).into_response(),
    }
}

fn packet_revision_json(revision: &tj_db::packets::PacketRevision) -> serde_json::Value {
    serde_json::json!({
        "id": revision.id,
        "packet_id": revision.packet_id,
        "run_id": revision.run_id,
        "revision": revision.revision,
        "schema_version": revision.schema_version,
        "content_hash": revision.content_hash,
        "completeness": revision.completeness,
        "parent_revision_id": revision.parent_revision_id,
        "source_artifact_id": revision.source_artifact_id,
        "source_artifact_hash": revision.source_artifact_hash,
        "flow_name": revision.flow_name,
        "stage_name": revision.stage_name,
        "change_request": revision.change_request,
        "created_by": revision.created_by,
        "created_at": revision.created_at,
        "content": serde_json::from_str::<serde_json::Value>(&revision.content_json).ok(),
    })
}

fn packet_review_json(review: &tj_db::packets::PacketReview) -> serde_json::Value {
    serde_json::json!({
        "id": review.id,
        "packet_id": review.packet_id,
        "revision_id": review.revision_id,
        "revision": review.revision,
        "content_hash": review.content_hash,
        "gate_instance_id": review.gate_instance_id,
        "decision_id": review.decision_id,
        "actor_id": review.actor_id,
        "actor_name": review.actor_name,
        "outcome": review.outcome,
        "reason": review.reason,
        "created_at": review.created_at,
    })
}

fn packet_provenance(revision: &tj_db::packets::PacketRevision) -> tj_packet::PacketProvenance {
    tj_packet::PacketProvenance {
        run_id: revision.run_id.clone(),
        flow_name: revision.flow_name.clone(),
        stage_name: revision.stage_name.clone(),
        artifact_id: revision.source_artifact_id.clone(),
        artifact_hash: revision.source_artifact_hash.clone(),
        created_by: revision.created_by.clone(),
        created_at: revision.created_at.clone(),
    }
}

fn packet_envelope(
    bundle: &tj_db::packets::PacketBundle,
    revision: &tj_db::packets::PacketRevision,
) -> Result<tj_packet::PacketEnvelope, String> {
    let packet = tj_packet::RequirementsPacket::from_json(&revision.content_json)
        .map_err(|error| error.to_string())?;
    let completeness = match revision.completeness.as_str() {
        "complete" => tj_packet::Completeness::Complete,
        "contradictory" => tj_packet::Completeness::Contradictory,
        _ => tj_packet::Completeness::Incomplete,
    };
    let history = bundle
        .revisions
        .iter()
        .map(|item| tj_packet::PacketRevisionSummary {
            revision_id: item.id.clone(),
            revision: item.revision,
            content_hash: item.content_hash.clone(),
            completeness: match item.completeness.as_str() {
                "complete" => tj_packet::Completeness::Complete,
                "contradictory" => tj_packet::Completeness::Contradictory,
                _ => tj_packet::Completeness::Incomplete,
            },
            parent_revision_id: item.parent_revision_id.clone(),
            change_request: item.change_request.clone(),
            created_by: item.created_by.clone(),
            created_at: item.created_at.clone(),
        })
        .collect();
    let reviews = bundle
        .reviews
        .iter()
        .map(|review| tj_packet::PacketReviewRecord {
            id: review.id.clone(),
            gate_instance_id: review.gate_instance_id.clone(),
            actor_id: review.actor_id.clone(),
            actor_name: review.actor_name.clone(),
            outcome: review.outcome.clone(),
            reason: review.reason.clone(),
            created_at: review.created_at.clone(),
        })
        .collect();
    Ok(tj_packet::PacketEnvelope {
        export_schema_version: 1,
        packet: tj_packet::PacketDocument {
            packet_id: revision.packet_id.clone(),
            revision: revision.revision,
            content_hash: revision.content_hash.clone(),
            completeness,
            packet,
        },
        provenance: packet_provenance(revision),
        reviews,
        history,
    })
}

async fn get_run_packet(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let user = match require_auth(&state, &headers) {
        Ok(user) => user,
        Err(r) => return r,
    };
    if !state
        .manager
        .db()
        .can_access_run(&user.id, &id)
        .unwrap_or(false)
    {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "Project access denied" })),
        )
            .into_response();
    }
    let bundle = match state.manager.db().packet_bundle(&id) {
        Ok(bundle) => bundle,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response();
        }
    };
    let gate_binding = state
        .manager
        .get_run(&id)
        .ok()
        .flatten()
        .and_then(|run| run.gate_instance_id().map(str::to_string))
        .and_then(|gate| state.manager.db().gate_packet_binding(&gate).ok().flatten());
    Json(serde_json::json!({
        "run_id": id,
        "packet_id": bundle.latest.as_ref().map(|revision| revision.packet_id.clone()),
        "latest": bundle.latest.as_ref().map(packet_revision_json),
        "revisions": bundle.revisions.iter().map(packet_revision_json).collect::<Vec<_>>(),
        "reviews": bundle.reviews.iter().map(packet_review_json).collect::<Vec<_>>(),
        "gate_binding": gate_binding.map(|binding| serde_json::json!({
            "gate_instance_id": binding.gate_instance_id,
            "packet_id": binding.packet_id,
            "revision_id": binding.revision_id,
            "revision": binding.revision,
            "content_hash": binding.content_hash,
            "completeness": binding.completeness,
        })),
        "allowed_source_references": state.manager.db().packet_allowed_sources(&id).unwrap_or_default(),
    })).into_response()
}

#[derive(Deserialize)]
struct SavePacketRevisionBody {
    content: serde_json::Value,
    #[serde(default)]
    packet_id: Option<String>,
    #[serde(default)]
    change_request: Option<String>,
}

async fn save_packet_revision(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<SavePacketRevisionBody>,
) -> impl IntoResponse {
    let user = match require_auth(&state, &headers) {
        Ok(user) => user,
        Err(r) => return r,
    };
    if !state
        .manager
        .db()
        .can_access_run(&user.id, &id)
        .unwrap_or(false)
    {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "Project access denied" })),
        )
            .into_response();
    }
    let change_request = body
        .change_request
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let allowed = match state.manager.db().packet_allowed_sources(&id) {
        Ok(allowed) => allowed,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response();
        }
    };
    let content_json = match serde_json::to_string(&body.content) {
        Ok(content) => content,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response();
        }
    };
    let draft = tj_db::packets::NewPacketRevision {
        run_id: id.clone(),
        packet_id: body.packet_id,
        content_json,
        parent_revision_id: None,
        source_artifact_id: None,
        source_artifact_hash: None,
        flow_name: None,
        stage_name: None,
        change_request: change_request.clone(),
        created_by: format!("user:{}", user.username),
        allowed_sources: Some(allowed),
    };
    match state.manager.db().insert_packet_revision(&draft) {
        Ok((revision, replayed)) => {
            let status = if replayed {
                StatusCode::OK
            } else {
                StatusCode::CREATED
            };
            let mut value = packet_revision_json(&revision);
            if let Some(object) = value.as_object_mut() {
                object.insert("replayed".into(), serde_json::Value::Bool(replayed));
            }
            (status, Json(value)).into_response()
        }
        Err(error) => {
            let message = error.to_string();
            let lower = message.to_lowercase();
            let status = if lower.contains("not found") {
                StatusCode::NOT_FOUND
            } else if lower.contains("cannot be written")
                || lower.contains("unchanged")
                || lower.contains("immutable")
            {
                StatusCode::CONFLICT
            } else {
                StatusCode::BAD_REQUEST
            };
            (status, Json(serde_json::json!({ "error": message }))).into_response()
        }
    }
}

async fn export_run_packet(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let user = match require_auth(&state, &headers) {
        Ok(user) => user,
        Err(r) => return r,
    };
    if !state
        .manager
        .db()
        .can_access_run(&user.id, &id)
        .unwrap_or(false)
    {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "Project access denied" })),
        )
            .into_response();
    }
    let format = query
        .get("format")
        .map(String::as_str)
        .unwrap_or("markdown");
    if !matches!(format, "markdown" | "md" | "json") {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "format must be markdown or json" })),
        )
            .into_response();
    }
    let bundle = match state.manager.db().packet_bundle(&id) {
        Ok(bundle) => bundle,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response();
        }
    };
    let revision =
        match query.get("revision") {
            Some(revision_id) => match state.manager.db().packet_revision(revision_id) {
                Ok(Some(revision)) if revision.run_id == id => revision,
                Ok(_) => return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({ "error": "packet revision not found for this run" })),
                )
                    .into_response(),
                Err(error) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({ "error": error.to_string() })),
                    )
                        .into_response();
                }
            },
            None => match bundle.latest.clone() {
                Some(revision) => revision,
                None => return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({ "error": "run has no requirements packet revision" })),
                )
                    .into_response(),
            },
        };
    let envelope = match packet_envelope(&bundle, &revision) {
        Ok(envelope) => envelope,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": error })),
            )
                .into_response();
        }
    };
    let json = format == "json";
    let body = if json {
        envelope.to_json_pretty()
    } else {
        envelope.to_markdown()
    };
    let extension = if json { "json" } else { "md" };
    let content_type = if json {
        "application/json; charset=utf-8"
    } else {
        "text/markdown; charset=utf-8"
    };
    let safe_packet_id: String = revision
        .packet_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let filename = format!("{safe_packet_id}-r{}.{extension}", revision.revision);
    let mut response = body.into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    if let Ok(disposition) = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
    {
        response
            .headers_mut()
            .insert(header::CONTENT_DISPOSITION, disposition);
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn plan_revision_json(revision: &tj_db::plans::PlanRevision) -> serde_json::Value {
    serde_json::json!({
        "id": revision.id,
        "plan_id": revision.plan_id,
        "run_id": revision.run_id,
        "revision": revision.revision,
        "schema_version": revision.schema_version,
        "content_hash": revision.content_hash,
        "packet_revision_id": revision.packet_revision_id,
        "packet_content_hash": revision.packet_content_hash,
        "packet_id": revision.packet_id,
        "packet_revision": revision.packet_revision,
        "parent_revision_id": revision.parent_revision_id,
        "source_artifact_id": revision.source_artifact_id,
        "source_artifact_hash": revision.source_artifact_hash,
        "flow_name": revision.flow_name,
        "stage_name": revision.stage_name,
        "change_request": revision.change_request,
        "created_by": revision.created_by,
        "created_at": revision.created_at,
        "content": serde_json::from_str::<serde_json::Value>(&revision.content_json).ok(),
        "validation": serde_json::from_str::<serde_json::Value>(&revision.validation_json)
            .unwrap_or(serde_json::Value::Null),
    })
}

fn plan_review_json(review: &tj_db::plans::PlanReview) -> serde_json::Value {
    serde_json::json!({
        "id": review.id,
        "plan_id": review.plan_id,
        "revision_id": review.revision_id,
        "revision": review.revision,
        "content_hash": review.content_hash,
        "packet_revision_id": review.packet_revision_id,
        "packet_content_hash": review.packet_content_hash,
        "gate_instance_id": review.gate_instance_id,
        "decision_id": review.decision_id,
        "actor_id": review.actor_id,
        "actor_name": review.actor_name,
        "outcome": review.outcome,
        "reason": review.reason,
        "created_at": review.created_at,
    })
}

fn plan_envelope(
    bundle: &tj_db::plans::PlanBundle,
    revision: &tj_db::plans::PlanRevision,
) -> Result<tj_plan::PlanEnvelope, String> {
    let plan =
        tj_plan::TaskPlan::from_json(&revision.content_json).map_err(|error| error.to_string())?;
    let validation: tj_plan::PlanValidationReport =
        serde_json::from_str(&revision.validation_json).map_err(|error| error.to_string())?;
    let history = bundle
        .revisions
        .iter()
        .map(|item| tj_plan::PlanRevisionSummary {
            revision_id: item.id.clone(),
            revision: item.revision,
            content_hash: item.content_hash.clone(),
            parent_revision_id: item.parent_revision_id.clone(),
            packet_revision_id: item.packet_revision_id.clone(),
            packet_content_hash: item.packet_content_hash.clone(),
            change_request: item.change_request.clone(),
            created_by: item.created_by.clone(),
            created_at: item.created_at.clone(),
        })
        .collect();
    let reviews = bundle
        .reviews
        .iter()
        .map(|review| tj_plan::PlanReviewRecord {
            id: review.id.clone(),
            gate_instance_id: review.gate_instance_id.clone(),
            actor_id: review.actor_id.clone(),
            actor_name: review.actor_name.clone(),
            outcome: review.outcome.clone(),
            reason: review.reason.clone(),
            created_at: review.created_at.clone(),
        })
        .collect();
    Ok(tj_plan::PlanEnvelope {
        export_schema_version: 1,
        plan: tj_plan::PlanDocument {
            plan_id: revision.plan_id.clone(),
            revision: revision.revision,
            content_hash: revision.content_hash.clone(),
            packet_revision_id: revision.packet_revision_id.clone(),
            packet_content_hash: revision.packet_content_hash.clone(),
            plan,
            validation,
        },
        provenance: tj_plan::PlanProvenance {
            run_id: revision.run_id.clone(),
            flow_name: revision.flow_name.clone(),
            stage_name: revision.stage_name.clone(),
            artifact_id: revision.source_artifact_id.clone(),
            artifact_hash: revision.source_artifact_hash.clone(),
            created_by: revision.created_by.clone(),
            created_at: revision.created_at.clone(),
        },
        reviews,
        history,
    })
}

fn plan_context_for(
    state: &Arc<AppState>,
    revision: &tj_db::plans::PlanRevision,
) -> Result<tj_plan::PlanContext, String> {
    let packet = state
        .manager
        .db()
        .packet_revision(&revision.packet_revision_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "plan source packet revision is missing".to_string())?;
    let value: serde_json::Value =
        serde_json::from_str(&packet.content_json).map_err(|error| error.to_string())?;
    Ok(tj_plan::PlanContext::from_packet_value(&value))
}

fn plan_readiness(
    state: &Arc<AppState>,
    revision: &tj_db::plans::PlanRevision,
    completed: &std::collections::HashSet<String>,
) -> Result<(tj_plan::TaskPlan, Vec<tj_plan::TaskReadiness>), String> {
    let plan =
        tj_plan::TaskPlan::from_json(&revision.content_json).map_err(|error| error.to_string())?;
    let context = plan_context_for(state, revision)?;
    let readiness = plan.readiness(completed, Some(&context));
    Ok((plan, readiness))
}

async fn get_run_plan(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let user = match require_auth(&state, &headers) {
        Ok(user) => user,
        Err(r) => return r,
    };
    if !state
        .manager
        .db()
        .can_access_run(&user.id, &id)
        .unwrap_or(false)
    {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "Project access denied" })),
        )
            .into_response();
    }
    let bundle = match state.manager.db().plan_bundle(&id) {
        Ok(bundle) => bundle,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response();
        }
    };
    let readiness = match bundle.latest.as_ref() {
        Some(revision) => match plan_readiness(&state, revision, &std::collections::HashSet::new())
        {
            Ok((_, readiness)) => readiness,
            Err(error) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": error })),
                )
                    .into_response();
            }
        },
        None => Vec::new(),
    };
    Json(serde_json::json!({
        "run_id": id,
        "plan_id": bundle.latest.as_ref().map(|revision| revision.plan_id.clone()),
        "latest": bundle.latest.as_ref().map(plan_revision_json),
        "approved": bundle.approved.as_ref().map(plan_revision_json),
        "revisions": bundle.revisions.iter().map(plan_revision_json).collect::<Vec<_>>(),
        "reviews": bundle.reviews.iter().map(plan_review_json).collect::<Vec<_>>(),
        "gate_binding": bundle.gate_binding.map(|binding| serde_json::json!({
            "gate_instance_id": binding.gate_instance_id,
            "plan_id": binding.plan_id,
            "revision_id": binding.revision_id,
            "revision": binding.revision,
            "content_hash": binding.content_hash,
            "packet_revision_id": binding.packet_revision_id,
            "packet_content_hash": binding.packet_content_hash,
        })),
        "readiness": readiness,
        "selections": state.manager.db().plan_task_selections(&id).unwrap_or_default().into_iter().map(|selection| serde_json::json!({
            "id": selection.id, "plan_revision_id": selection.plan_revision_id,
            "plan_content_hash": selection.plan_content_hash, "task_id": selection.task_id,
            "selected_by": selection.selected_by, "created_at": selection.created_at,
        })).collect::<Vec<_>>(),
    })).into_response()
}

#[derive(Deserialize)]
struct SavePlanRevisionBody {
    content: serde_json::Value,
    #[serde(default)]
    plan_id: Option<String>,
    #[serde(default)]
    change_request: Option<String>,
}

async fn save_plan_revision(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<SavePlanRevisionBody>,
) -> impl IntoResponse {
    let user = match require_auth(&state, &headers) {
        Ok(user) => user,
        Err(r) => return r,
    };
    if !state
        .manager
        .db()
        .can_access_run(&user.id, &id)
        .unwrap_or(false)
    {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "Project access denied" })),
        )
            .into_response();
    }
    let change_request = body
        .change_request
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let content_json = match serde_json::to_string(&body.content) {
        Ok(content) => content,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response();
        }
    };
    let draft = tj_db::plans::NewPlanRevision {
        run_id: id.clone(),
        plan_id: body.plan_id,
        content_json,
        parent_revision_id: None,
        source_packet_revision_id: None,
        source_artifact_id: None,
        source_artifact_hash: None,
        flow_name: None,
        stage_name: None,
        change_request: change_request.clone(),
        created_by: format!("user:{}", user.username),
    };
    match state.manager.db().insert_plan_revision(&draft) {
        Ok((revision, replayed)) => {
            let status = if replayed {
                StatusCode::OK
            } else {
                StatusCode::CREATED
            };
            let mut value = plan_revision_json(&revision);
            if let Some(object) = value.as_object_mut() {
                object.insert("replayed".into(), serde_json::Value::Bool(replayed));
            }
            (status, Json(value)).into_response()
        }
        Err(error) => {
            let message = error.to_string();
            let lower = message.to_lowercase();
            let status = if lower.contains("not found") {
                StatusCode::NOT_FOUND
            } else if lower.contains("cannot be written")
                || lower.contains("unchanged")
                || lower.contains("immutable")
                || lower.contains("stale")
            {
                StatusCode::CONFLICT
            } else {
                StatusCode::BAD_REQUEST
            };
            (status, Json(serde_json::json!({
                "error": message,
                "requires_packet_revision": message.contains("requirements packet revision") || message.contains("unknown requirement"),
            }))).into_response()
        }
    }
}

async fn export_run_plan(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let user = match require_auth(&state, &headers) {
        Ok(user) => user,
        Err(r) => return r,
    };
    if !state
        .manager
        .db()
        .can_access_run(&user.id, &id)
        .unwrap_or(false)
    {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "Project access denied" })),
        )
            .into_response();
    }
    let format = query
        .get("format")
        .map(String::as_str)
        .unwrap_or("markdown");
    if !matches!(format, "markdown" | "md" | "json") {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "format must be markdown or json" })),
        )
            .into_response();
    }
    let bundle = match state.manager.db().plan_bundle(&id) {
        Ok(bundle) => bundle,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response();
        }
    };
    let revision = match query.get("revision") {
        Some(revision_id) => match state.manager.db().plan_revision(revision_id) {
            Ok(Some(revision)) if revision.run_id == id => revision,
            Ok(_) => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({ "error": "plan revision not found for this run" })),
                )
                    .into_response();
            }
            Err(error) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": error.to_string() })),
                )
                    .into_response();
            }
        },
        None => match bundle.latest.clone() {
            Some(revision) => revision,
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({ "error": "run has no structured plan revision" })),
                )
                    .into_response();
            }
        },
    };
    let envelope = match plan_envelope(&bundle, &revision) {
        Ok(envelope) => envelope,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": error })),
            )
                .into_response();
        }
    };
    let json = format == "json";
    let body = if json {
        envelope.to_json_pretty()
    } else {
        envelope.to_markdown()
    };
    let extension = if json { "json" } else { "md" };
    let content_type = if json {
        "application/json; charset=utf-8"
    } else {
        "text/markdown; charset=utf-8"
    };
    let safe_plan_id: String = revision
        .plan_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let filename = format!("{safe_plan_id}-r{}.{extension}", revision.revision);
    let mut response = body.into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    if let Ok(disposition) = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
    {
        response
            .headers_mut()
            .insert(header::CONTENT_DISPOSITION, disposition);
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn parse_completed(raw: Option<&String>) -> std::collections::HashSet<String> {
    raw.map(|value| {
        value
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default()
}

async fn get_plan_tasks(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let user = match require_auth(&state, &headers) {
        Ok(user) => user,
        Err(r) => return r,
    };
    if !state
        .manager
        .db()
        .can_access_run(&user.id, &id)
        .unwrap_or(false)
    {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "Project access denied" })),
        )
            .into_response();
    }
    let revision = match query.get("revision") {
        Some(revision_id) => match state.manager.db().plan_revision(revision_id) {
            Ok(Some(revision)) if revision.run_id == id => revision,
            Ok(_) => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({ "error": "plan revision not found for this run" })),
                )
                    .into_response();
            }
            Err(error) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": error.to_string() })),
                )
                    .into_response();
            }
        },
        None => match state
            .manager
            .db()
            .plan_bundle(&id)
            .ok()
            .and_then(|bundle| bundle.latest)
        {
            Some(revision) => revision,
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({ "error": "run has no structured plan revision" })),
                )
                    .into_response();
            }
        },
    };
    let completed = parse_completed(query.get("completed"));
    let (plan, readiness) = match plan_readiness(&state, &revision, &completed) {
        Ok(value) => value,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": error })),
            )
                .into_response();
        }
    };
    let ready_task_ids: Vec<String> = readiness
        .iter()
        .filter(|task| task.ready)
        .map(|task| task.task_id.clone())
        .collect();
    Json(serde_json::json!({
        "run_id": id,
        "plan_id": revision.plan_id,
        "revision_id": revision.id,
        "revision": revision.revision,
        "content_hash": revision.content_hash,
        "packet_revision_id": revision.packet_revision_id,
        "packet_content_hash": revision.packet_content_hash,
        "completed_task_ids": completed,
        "ready_task_ids": ready_task_ids,
        "tasks": readiness,
        "scope_expansion_requests": plan.scope_expansion_requests,
        "requires_packet_revision": !plan.scope_expansion_requests.is_empty(),
    }))
    .into_response()
}

#[derive(Deserialize)]
struct SelectPlanTaskBody {
    plan_revision_id: String,
    plan_content_hash: String,
    task_id: String,
    #[serde(default)]
    completed_task_ids: Vec<String>,
}

async fn select_plan_task(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<SelectPlanTaskBody>,
) -> impl IntoResponse {
    let user = match require_auth(&state, &headers) {
        Ok(user) => user,
        Err(r) => return r,
    };
    if !state
        .manager
        .db()
        .can_access_run(&user.id, &id)
        .unwrap_or(false)
    {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "Project access denied" })),
        )
            .into_response();
    }
    let revision = match state.manager.db().plan_revision(&body.plan_revision_id) {
        Ok(Some(revision)) if revision.run_id == id => revision,
        Ok(_) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "plan revision not found for this run" })),
            )
                .into_response();
        }
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response();
        }
    };
    if revision.content_hash != body.plan_content_hash {
        return (StatusCode::CONFLICT, Json(serde_json::json!({
            "error": "Selected plan revision/hash is stale; refresh the approved plan before selecting a task"
        }))).into_response();
    }
    // Task selection cannot trust a caller-supplied "completed" set:
    // that would let a client declare an unmet dependency complete and release
    // work the approved plan still blocks. There is no durable task-completion
    // ledger in this local slice, so only dependency-free ready tasks can be
    // selected; a future cross-task workflow must add durable completion
    // evidence first.
    if !body.completed_task_ids.is_empty() {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "completed_task_ids cannot be asserted by the selection caller; only durable completed-task evidence may satisfy dependencies, and this slice has no such ledger"
            })),
        )
            .into_response();
    }
    let completed: std::collections::HashSet<String> = std::collections::HashSet::new();
    let (plan, readiness) = match plan_readiness(&state, &revision, &completed) {
        Ok(value) => value,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": error })),
            )
                .into_response();
        }
    };
    let Some(task) = plan.task(&body.task_id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "task not found in the selected plan revision" })),
        )
            .into_response();
    };
    let Some(task_readiness) = readiness.iter().find(|task| task.task_id == body.task_id) else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "task readiness missing" })),
        )
            .into_response();
    };
    if !task_readiness.ready {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "Task is blocked and cannot be selected for the next flow",
                "blocked_reasons": task_readiness.blocked_reasons,
            })),
        )
            .into_response();
    }
    let payload = serde_json::json!({
        "run_id": id,
        "plan": {
            "plan_id": revision.plan_id, "revision_id": revision.id,
            "revision": revision.revision, "content_hash": revision.content_hash,
            "packet_revision_id": revision.packet_revision_id,
            "packet_content_hash": revision.packet_content_hash,
            "provenance": {
                "flow_name": revision.flow_name, "stage_name": revision.stage_name,
                "artifact_id": revision.source_artifact_id,
                "artifact_hash": revision.source_artifact_hash,
                "created_by": revision.created_by, "created_at": revision.created_at,
            },
        },
        "packet": {
            "packet_id": revision.packet_id,
            "revision": revision.packet_revision,
            "content_hash": revision.packet_content_hash,
        },
        "task": task,
        "readiness": task_readiness,
        "completed_task_ids": completed,
    });
    let payload_json = payload.to_string();
    match state.manager.db().insert_plan_task_selection(
        &id,
        &revision,
        &body.task_id,
        &format!("user:{}", user.username),
        payload_json,
    ) {
        Ok((selection, replayed)) => {
            let released = state
                .manager
                .notify_task_selected(&id, &selection.id)
                .is_ok();
            let status = if replayed {
                StatusCode::OK
            } else {
                StatusCode::CREATED
            };
            let mut response = Json(serde_json::json!({
                "selection": selection,
                "payload": serde_json::from_str::<serde_json::Value>(&selection.payload_json).unwrap_or(serde_json::Value::Null),
                "replayed": replayed,
                "task_selection_recorded": released,
            })).into_response();
            *response.status_mut() = status;
            response
        }
        Err(error) => {
            let message = error.to_string();
            let lower = message.to_lowercase();
            let status = if lower.contains("approved plan") || lower.contains("not found") {
                StatusCode::CONFLICT
            } else {
                StatusCode::BAD_REQUEST
            };
            (status, Json(serde_json::json!({ "error": message }))).into_response()
        }
    }
}

async fn get_run_budget(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let user = match require_auth(&state, &headers) {
        Ok(user) => user,
        Err(r) => return r,
    };
    if !state
        .manager
        .db()
        .can_access_run(&user.id, &id)
        .unwrap_or(false)
    {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "Project access denied" })),
        )
            .into_response();
    }
    match state.manager.budget(&id) {
        Ok(Some(budget)) => Json(budget).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "run has no budget record (legacy run)" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn get_run_recovery(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let user = match require_auth(&state, &headers) {
        Ok(user) => user,
        Err(r) => return r,
    };
    if !state
        .manager
        .db()
        .can_access_run(&user.id, &id)
        .unwrap_or(false)
    {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "Project access denied" })),
        )
            .into_response();
    }
    match (
        state.manager.recovery(&id),
        state.manager.recovery_effects(&id),
    ) {
        (Ok(recovery), Ok(effects)) => Json(serde_json::json!({
            "recovery": recovery,
            "unresolved_effects": effects,
        }))
        .into_response(),
        (Err(e), _) | (_, Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn retry_run(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<RetryRunBody>,
) -> impl IntoResponse {
    let actor = match require_auth(&state, &headers) {
        Ok(user) => user,
        Err(r) => return r,
    };
    if !state
        .manager
        .db()
        .can_access_run(&actor.id, &id)
        .unwrap_or(false)
    {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "Project access denied" })),
        )
            .into_response();
    }
    match state
        .manager
        .retry_run(&id, body.acknowledge_unknown_effects)
        .await
    {
        Ok(plan) => {
            tracing::info!(run_id = %id, actor = %actor.username, flow = %plan.flow_name, "Explicit retry accepted");
            (
                StatusCode::ACCEPTED,
                Json(serde_json::json!({ "accepted": true, "plan": plan })),
            )
                .into_response()
        }
        Err(error) => {
            let message = error.to_string();
            let status = if message.contains("not found") {
                StatusCode::NOT_FOUND
            } else if message.contains("unknown effects") || message.contains("only interrupted") {
                StatusCode::CONFLICT
            } else {
                StatusCode::BAD_REQUEST
            };
            (status, Json(serde_json::json!({ "error": message }))).into_response()
        }
    }
}

async fn get_run_input(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = require_auth(&state, &headers) {
        return r;
    }
    let transcript = state.manager.web_transcript(&id);
    let waiting = state
        .manager
        .db()
        .run_has_waiting_stage(&id)
        .unwrap_or(false);
    let queued = state.manager.web_input_queued(&id);
    Json(serde_json::json!({
        "bridge_active": transcript.is_some(),
        "waiting": waiting,
        "stage_messages": transcript.unwrap_or_default(),
        "queued": queued.iter().map(|(seq, text)| serde_json::json!({ "seq": seq, "text": text })).collect::<Vec<_>>(),
    })).into_response()
}

async fn list_run_files(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = require_auth(&state, &headers) {
        return r;
    }
    match state.manager.list_run_files(&id) {
        Ok(files) => Json(serde_json::json!({ "files": files })).into_response(),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": e })),
        )
            .into_response(),
    }
}

async fn read_run_file(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
    Path((id, path)): Path<(String, String)>,
) -> impl IntoResponse {
    if let Err(r) = require_auth(&state, &headers) {
        return r;
    }
    if params.get("raw").map(|v| v == "1").unwrap_or(false) {
        // Base64 mode for binary previews (images, etc).
        let bytes = state.manager.read_run_file_bytes(&id, &path);
        return match bytes {
            Ok(bytes) => {
                use base64::Engine as _;
                let mime = match path
                    .rsplit('.')
                    .next()
                    .unwrap_or("")
                    .to_lowercase()
                    .as_str()
                {
                    "png" => "image/png",
                    "jpg" | "jpeg" => "image/jpeg",
                    "gif" => "image/gif",
                    "webp" => "image/webp",
                    "svg" => "image/svg+xml",
                    "ico" => "image/x-icon",
                    "pdf" => "application/pdf",
                    _ => "application/octet-stream",
                };
                Json(serde_json::json!({ "path": path, "mime": mime, "base64": base64::engine::general_purpose::STANDARD.encode(bytes) })).into_response()
            }
            Err(e) => (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": e })),
            )
                .into_response(),
        };
    }
    match state.manager.read_run_file(&id, &path) {
        Ok(content) => (StatusCode::OK, content).into_response(),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": e })),
        )
            .into_response(),
    }
}

async fn delete_chat(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(chat_id): Path<String>,
) -> impl IntoResponse {
    let actor = match require_auth(&state, &headers) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let session_id = crate::auth::chat_session_id(&actor.id, &chat_id);
    state.sessions.remove(&session_id);
    (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response()
}

async fn get_run_artifacts(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = require_auth(&state, &headers) {
        return r;
    }
    match state.manager.list_artifacts(&id) {
        Ok(list) => Json(serde_json::to_value(&list).unwrap_or_default()).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn fork_artifact(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(artifact_id): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = require_auth(&state, &headers) {
        return r;
    }
    match state.manager.fork_artifact(&artifact_id) {
        Ok(f) => (
            StatusCode::CREATED,
            Json(serde_json::to_value(&f).unwrap_or_default()),
        )
            .into_response(),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn get_run_decisions(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = require_auth(&state, &headers) {
        return r;
    }
    match state.manager.list_gate_decisions(&id) {
        Ok(list) => Json(serde_json::to_value(&list).unwrap_or_default()).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct CreateProjectBody {
    name: String,
    #[serde(default)]
    repo_url: Option<String>,
    #[serde(default)]
    base_branch: Option<String>,
    /// Repository provider identity. It is not applicable to a workflow that
    /// declares no repository workspace; the manager fails closed instead of
    /// silently ignoring it.
    #[serde(default)]
    git_provider: Option<serde_json::Value>,
    /// Preferred exact immutable registry version.
    #[serde(default)]
    workflow_id: Option<String>,
    /// Registered catalog identity; resolves to the highest imported revision.
    #[serde(default)]
    catalog_id: Option<String>,
    /// Legacy alias. It resolves only when it names a registered catalog id.
    #[serde(default)]
    profile: Option<String>,
}

async fn create_project(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<CreateProjectBody>,
) -> impl IntoResponse {
    if let Err(r) = require_auth(&state, &headers) {
        return r;
    }
    if body.name.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "name is required" })),
        )
            .into_response();
    }
    let manager = state.manager.clone();
    let name = body.name.trim().to_string();
    let repo = body
        .repo_url
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string());
    let branch = body
        .base_branch
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string());
    // Validate the provider shape here so an invalid body fails as a request
    // error rather than a provisioned project that can never publish. The
    // manager re-validates before persisting.
    let git_provider_json = match body.git_provider {
        Some(value) if !value.is_null() => {
            match serde_json::from_value::<tj_git::GitProviderConfig>(value) {
                Ok(config) => serde_json::to_string(&config).ok(),
                Err(error) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({
                            "error": format!("git_provider is invalid: {error}")
                        })),
                    )
                        .into_response();
                }
            }
        }
        _ => None,
    };
    let workflow_id = body.workflow_id.clone();
    let catalog_id = body.catalog_id.clone();
    let legacy_profile = body.profile.clone();
    // Resolve and create together with the same immutable registry version.
    // This may create a workspace, so keep it off async workers.
    let created = tokio::task::spawn_blocking(move || {
        let workflow = manager.resolve_project_workflow(
            workflow_id.as_deref(),
            catalog_id.as_deref(),
            legacy_profile.as_deref(),
        )?;
        manager
            .create_project_with_workflow_and_lane(
                &name,
                repo.as_deref(),
                branch.as_deref(),
                git_provider_json.as_deref(),
                &workflow,
            )
            .map(|(project, lane)| (project, lane, workflow.id))
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|r| r.map_err(|e| e.to_string()));
    match created {
        Ok((project, lane, workflow_id)) => (
            StatusCode::CREATED,
            Json(serde_json::json!({
                "project": project,
                "lane": lane,
                "workflow_id": workflow_id,
            })),
        )
            .into_response(),
        Err(error) => {
            let status = if error.starts_with("No workflow installed") {
                StatusCode::CONFLICT
            } else {
                StatusCode::BAD_REQUEST
            };
            (status, Json(serde_json::json!({ "error": error }))).into_response()
        }
    }
}

async fn get_profiles(State(state): State<Arc<AppState>>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(r) = require_auth(&state, &headers) {
        return r;
    }
    let workflows = match state.manager.list_workflows() {
        Ok(workflows) => workflows,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response();
        }
    };
    let catalog = match state.manager.db().list_catalog() {
        Ok(catalog) => catalog,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response();
        }
    };
    let items: Vec<serde_json::Value> = catalog
        .iter()
        .map(|entry| {
            let graph = serde_json::from_str::<tj_config::smg::StateMachineGraph>(
                &entry.content_json,
            )
            .ok();
            // Catalog metadata is a summary of the actual validated declaration,
            // never a temporary generic-safe or profile-derived claim.
            let contract = graph.as_ref().map(tj_config::capability::effective_contract);
            let repository_free = contract
                .as_ref()
                .is_none_or(|contract| contract.workspace_mode() == tj_config::capability::WorkspaceMode::None);
            let gate_binding = graph
                .as_ref()
                .and_then(|graph| {
                    graph.flow_order.iter().find_map(|flow| {
                        graph
                            .flows
                            .get(flow)
                            .filter(|flow| flow.flow_type == tj_config::smg::FlowType::Gate)
                            .map(|_| flow.as_str())
                    })
                })
                .map(|gate| {
                    contract
                        .as_ref()
                        .map(|contract| contract.gate_binding(gate).as_str())
                        .unwrap_or("artifact")
                })
                .unwrap_or("artifact");
            let label = entry
                .catalog_label
                .clone()
                .or_else(|| entry.catalog_id.clone())
                .unwrap_or_else(|| entry.name.clone());
            let description = entry
                .catalog_description
                .clone()
                .unwrap_or_else(|| entry.description.clone().unwrap_or_default());
            let versions: Vec<_> = workflows
                .iter()
                .filter(|wf| wf.catalog_id == entry.catalog_id)
                .map(|wf| {
                    serde_json::json!({
                        "workflow_id": wf.id,
                        "revision": wf.revision,
                        "content_hash": wf.content_hash,
                    })
                })
                .collect();
            serde_json::json!({
                "id": entry.catalog_id,
                "catalog_id": entry.catalog_id,
                "label": label,
                "description": description,
                "workflow_id": entry.id,
                "smg_path": entry.source_rel_path.as_deref().unwrap_or(&entry.smg_path),
                "maturity": graph.as_ref().map(|g| g.maturity).unwrap_or(tj_config::smg::WorkflowMaturity::Experimental),
                "repository_free": repository_free,
                "gate_binding": gate_binding,
                "default": entry.catalog_default,
                "versions": versions,
            })
        })
        .collect();
    Json(serde_json::json!({ "profiles": items, "catalog_format_version": 1 })).into_response()
}

/// Normalized review identity for the active gate. Durable binding rows are the
/// authority; the frozen contract is not re-read here. `kind` is the only field
/// UI panels may branch on.
fn normalized_gate_binding(
    db: &tj_db::Database,
    gate_instance_id: Option<&str>,
) -> Option<serde_json::Value> {
    let gate = gate_instance_id.filter(|gate| !gate.trim().is_empty())?;
    if let Ok(Some(binding)) = db.gate_plan_binding(gate) {
        return Some(serde_json::json!({
            "kind": "plan",
            "gate_instance_id": binding.gate_instance_id,
            "flow_name": serde_json::Value::Null,
            "stage_name": serde_json::Value::Null,
            "artifact_id": serde_json::Value::Null,
            "artifact_version": serde_json::Value::Null,
            "content_hash": binding.content_hash,
            "packet_revision_id": binding.packet_revision_id,
            "packet_content_hash": binding.packet_content_hash,
            "plan_revision_id": binding.revision_id,
            "plan_content_hash": binding.content_hash,
        }));
    }
    if let Ok(Some(binding)) = db.gate_packet_binding(gate) {
        return Some(serde_json::json!({
            "kind": "packet",
            "gate_instance_id": binding.gate_instance_id,
            "flow_name": serde_json::Value::Null,
            "stage_name": serde_json::Value::Null,
            "artifact_id": serde_json::Value::Null,
            "artifact_version": serde_json::Value::Null,
            "content_hash": binding.content_hash,
            "packet_revision_id": binding.revision_id,
            "packet_content_hash": binding.content_hash,
            "plan_revision_id": serde_json::Value::Null,
            "plan_content_hash": serde_json::Value::Null,
        }));
    }
    if let Ok(Some(binding)) = db.gate_output_binding(gate) {
        return Some(serde_json::json!({
            "kind": "artifact",
            "gate_instance_id": binding.gate_instance_id,
            "flow_name": binding.flow_name,
            "stage_name": binding.stage_name,
            "artifact_id": binding.artifact_id,
            "artifact_version": binding.artifact_version,
            "content_hash": binding.content_hash,
            "packet_revision_id": serde_json::Value::Null,
            "packet_content_hash": serde_json::Value::Null,
            "plan_revision_id": serde_json::Value::Null,
            "plan_content_hash": serde_json::Value::Null,
        }));
    }
    None
}

fn artifact_json(artifact: &tj_db::RunArtifact) -> serde_json::Value {
    serde_json::json!({
        "id": artifact.id,
        "run_id": artifact.run_id,
        "flow_name": artifact.flow_name,
        "stage_name": artifact.stage_name,
        "attempt": artifact.attempt,
        "version": artifact.version,
        "content_hash": artifact.content_hash,
        "created_at": artifact.created_at,
        "content": artifact.content,
        "value": serde_json::from_str::<serde_json::Value>(&artifact.content)
            .unwrap_or_else(|_| serde_json::Value::String(artifact.content.clone())),
    })
}

async fn get_run_output(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let user = match require_auth(&state, &headers) {
        Ok(user) => user,
        Err(r) => return r,
    };
    if !state
        .manager
        .db()
        .can_access_run(&user.id, &id)
        .unwrap_or(false)
    {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "Project access denied" })),
        )
            .into_response();
    }
    let run = match state.manager.get_run(&id) {
        Ok(Some(run)) => run,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "Job not found" })),
            )
                .into_response();
        }
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response();
        }
    };
    let gate = run.gate_instance_id();
    let binding = normalized_gate_binding(state.manager.db(), gate);
    let latest = state
        .manager
        .db()
        .list_artifacts(&id)
        .ok()
        .and_then(|items| items.into_iter().max_by_key(|item| item.created_at.clone()));
    let decisions = state.manager.list_gate_decisions(&id).unwrap_or_default();
    let project = run
        .project_id()
        .and_then(|pid| state.manager.db().load_project(pid.as_str()).ok().flatten());
    Json(serde_json::json!({
        "run_id": id,
        "catalog_id": project.as_ref().and_then(|p| p.catalog_id.clone()),
        "profile": project.as_ref().map(|p| p.profile.clone()),
        "gate_name": run.gate_name(),
        "gate_binding": binding,
        "latest": latest.as_ref().map(artifact_json),
        "decisions": decisions,
    }))
    .into_response()
}

async fn get_workflows(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let user = match require_auth(&state, &headers) {
        Ok(user) => user,
        Err(r) => return r,
    };
    match state.manager.list_workflows() {
        Ok(versions) => {
            let items: Vec<serde_json::Value> = versions
                .iter()
                .map(|wf| {
                    let lanes = state
                        .manager
                        .lanes_using_workflow(&wf.id)
                        .unwrap_or_default();
                    let lanes: Vec<_> = lanes
                        .into_iter()
                        .filter(|l| {
                            state
                                .manager
                                .db()
                                .can_access_project(&user.id, l.project_id.as_str())
                                .unwrap_or(false)
                        })
                        .collect();
                    workflow_json(
                        wf,
                        &lanes,
                        state.manager.db().workflow_asset_count(&wf.id).unwrap_or(0),
                    )
                })
                .collect();
            let configured = state
                .config_loader
                .configured_workflow_sources()
                .ok()
                .filter(|sources| !sources.is_empty())
                .map(|sources| {
                    serde_json::json!({
                        "sources": sources.into_iter().map(|source| serde_json::json!({
                            "id": source.id,
                            "path": source.path,
                            "kind": source.kind,
                        })).collect::<Vec<_>>()
                    })
                });
            Json(serde_json::json!({ "workflows": items, "configured": configured }))
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn get_workflow_detail(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let user = match require_auth(&state, &headers) {
        Ok(user) => user,
        Err(r) => return r,
    };
    match state.manager.get_workflow(&id) {
        Ok(Some(wf)) => {
            let lanes = state
                .manager
                .lanes_using_workflow(&wf.id)
                .unwrap_or_default();
            let lanes: Vec<_> = lanes
                .into_iter()
                .filter(|l| {
                    state
                        .manager
                        .db()
                        .can_access_project(&user.id, l.project_id.as_str())
                        .unwrap_or(false)
                })
                .collect();
            Json(workflow_json(
                &wf,
                &lanes,
                state.manager.db().workflow_asset_count(&wf.id).unwrap_or(0),
            ))
            .into_response()
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "workflow not found" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn import_workflows(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(r) = require_auth(&state, &headers) {
        return r;
    }
    match state.manager.import_workflows() {
        Ok(versions) => {
            let items: Vec<serde_json::Value> = versions
                .iter()
                .map(|wf| {
                    let lanes = state
                        .manager
                        .lanes_using_workflow(&wf.id)
                        .unwrap_or_default();
                    workflow_json(
                        wf,
                        &lanes,
                        state.manager.db().workflow_asset_count(&wf.id).unwrap_or(0),
                    )
                })
                .collect();
            Json(serde_json::json!({ "workflows": items })).into_response()
        }
        Err(e) => {
            let message = format!("{e:#}");
            let status = if message.contains("validation")
                || message.contains("cannot be imported")
                || message.contains("capabilit")
                || message.contains("catalog")
                || message.contains("unknown field")
                || message.contains("schema_version")
                || message.contains("package asset")
                || message.contains("workflow.smg.json")
                || message.contains("agent reference")
            {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (status, Json(serde_json::json!({ "error": message }))).into_response()
        }
    }
}

fn workflow_json(
    wf: &tj_db::WorkflowVersion,
    lanes: &[tj_db::Lane],
    asset_count: i64,
) -> serde_json::Value {
    serde_json::json!({
        "id": wf.id,
        "name": wf.name,
        "smg_path": wf.smg_path,
        "source_id": wf.source_id,
        "source_rel_path": wf.source_rel_path,
        "source_kind": wf.source_kind,
        "asset_count": asset_count,
        "version": wf.version,
        "revision": wf.revision,
        "description": wf.description,
        "catalog_id": wf.catalog_id,
        "catalog_label": wf.catalog_label,
        "catalog_description": wf.catalog_description,
        "catalog_default": wf.catalog_default,
        "capability_schema_version": wf.capability_schema_version,
        "capability_hash": wf.capability_hash,
        "profile": serde_json::Value::Null,
        "maturity": serde_json::from_str::<tj_config::smg::StateMachineGraph>(&wf.content_json)
            .map(|graph| graph.maturity).unwrap_or(tj_config::smg::WorkflowMaturity::Experimental),
        "entry": wf.entry,
        "exit": wf.exit,
        "flow_count": wf.flow_count,
        "gate_count": wf.gate_count,
        "stage_count": wf.stage_count,
        "created_at": wf.created_at,
        "content": serde_json::from_str::<serde_json::Value>(&wf.content_json).unwrap_or(serde_json::Value::Null),
        "lanes": lanes.iter().map(|l| serde_json::json!({
            "id": l.id, "project_id": l.project_id, "name": l.name,
        })).collect::<Vec<_>>(),
    })
}

async fn list_lanes(State(state): State<Arc<AppState>>, headers: HeaderMap) -> impl IntoResponse {
    let user = match require_auth(&state, &headers) {
        Ok(user) => user,
        Err(r) => return r,
    };
    match state.manager.list_lanes() {
        Ok(lanes) => Json(serde_json::json!({ "lanes": lanes.into_iter().filter(|l| state.manager.db().can_access_project(&user.id, l.project_id.as_str()).unwrap_or(false)).collect::<Vec<_>>() })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": e.to_string() }))).into_response(),
    }
}

#[derive(serde::Deserialize)]
struct PinWorkflowBody {
    workflow_id: Option<String>,
}

async fn pin_lane_workflow(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<PinWorkflowBody>,
) -> impl IntoResponse {
    let user = match require_auth(&state, &headers) {
        Ok(user) => user,
        Err(r) => return r,
    };
    if body
        .workflow_id
        .as_deref()
        .is_none_or(|id| id.trim().is_empty())
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "a lane must stay pinned to a registry workflow; provide workflow_id (unpinning is refused)"
            })),
        )
            .into_response();
    }
    let lane = match state.manager.db().load_lane(&id) {
        Ok(Some(lane)) => lane,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "lane not found" })),
            )
                .into_response();
        }
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response();
        }
    };
    if !state
        .manager
        .db()
        .can_access_project(&user.id, lane.project_id.as_str())
        .unwrap_or(false)
    {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "Project access denied" })),
        )
            .into_response();
    }
    match state
        .manager
        .set_lane_workflow(&id, body.workflow_id.as_deref())
    {
        Ok(()) => {
            let lane = state.manager.db().load_lane(&id).ok().flatten();
            Json(serde_json::json!({ "lane": lane })).into_response()
        }
        Err(e) if e.to_string().contains("does not exist") => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
        Err(e) if e.to_string().contains("workspace policy is incompatible") => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

fn parse_artifacts(raw: Option<&str>) -> serde_json::Value {
    match raw {
        Some(s) if !s.trim().is_empty() => serde_json::from_str::<serde_json::Value>(s)
            .unwrap_or_else(|_| serde_json::Value::String(s.to_string())),
        _ => serde_json::Value::Null,
    }
}

async fn get_run_tree(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = require_auth(&state, &headers) {
        return r;
    }
    let run = match state.manager.get_run(&id) {
        Ok(Some(r)) => r,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "Job not found" })),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };
    let flows = match state.manager.db().load_flows_for_run(&id) {
        Ok(f) => f,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };
    // Keep DB (rowid) order: flow_number is a cross-run counter, not execution order.
    let mut flow_json = Vec::with_capacity(flows.len());
    for f in flows {
        let stages = state
            .manager
            .db()
            .load_stages_for_flow(f.id())
            .unwrap_or_default()
            .into_iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.id(),
                    "stage_name": s.stage_name(),
                    "status": s.status(),
                    "iteration": s.iteration(),
                    "error": s.error(),
                    "started_at": s.started_at(),
                    "completed_at": s.completed_at(),
                    "input_artifacts": parse_artifacts(s.input_artifacts()),
                    "output_artifacts": parse_artifacts(s.output_artifacts()),
                })
            })
            .collect::<Vec<_>>();
        flow_json.push(serde_json::json!({
            "id": f.id(),
            "flow_name": f.flow_name(),
            "flow_number": f.flow_number(),
            "status": f.status(),
            "branch_name": f.branch_name(),
            "started_at": f.started_at(),
            "completed_at": f.completed_at(),
            "stages": stages,
        }));
    }
    let mut run_json = serde_json::to_value(&run).unwrap_or_else(|_| serde_json::json!({}));
    let project = run.project_id().and_then(|project_id| {
        state
            .manager
            .db()
            .load_project(project_id.as_str())
            .ok()
            .flatten()
    });
    if let Some(object) = run_json.as_object_mut() {
        if let Some(project) = &project {
            object.insert(
                "catalog_id".into(),
                project
                    .catalog_id
                    .clone()
                    .map(serde_json::Value::String)
                    .unwrap_or(serde_json::Value::Null),
            );
            object.insert(
                "profile".into(),
                serde_json::Value::String(project.profile.clone()),
            );
        }
        object.insert(
            "gate_binding".into(),
            normalized_gate_binding(state.manager.db(), run.gate_instance_id())
                .unwrap_or(serde_json::Value::Null),
        );
    }
    Json(serde_json::json!({ "run": run_json, "flows": flow_json })).into_response()
}

async fn cancel_run(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = require_auth(&state, &headers) {
        return r;
    }
    match state.manager.cancel_run(&id).await {
        Ok(()) => match state.manager.get_run(&id) {
            Ok(Some(run)) => Json(serde_json::to_value(&run).unwrap()).into_response(),
            Ok(None) => (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "Job not found" })),
            )
                .into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response(),
        },
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn merge_complete(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = require_auth(&state, &headers) {
        return r;
    }
    // Merge is now handled internally by the worktree lifecycle in ManagerActor.
    // This endpoint remains for API compatibility but is a no-op.

    match state.manager.get_run(&id) {
        Ok(Some(run)) => Json(serde_json::to_value(&run).unwrap()).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "Job not found" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn sse_pipeline_events(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    let authorized = match params.get("token") {
        Some(t) => matches!(state.manager.user_for_token(t), Ok(Some(_))),
        None => false,
    };
    if !authorized {
        return sse_unauthorized();
    }
    let rx = state.pipeline_events.subscribe();
    let token = params.get("token").cloned().unwrap_or_default();
    let stream = crate::access::pipeline_events(state.manager.db().clone(), token, rx);

    Sse::new(Box::pin(stream) as EventStream)
        .keep_alive(
            axum::response::sse::KeepAlive::new()
                .interval(std::time::Duration::from_secs(30))
                .text("ping"),
        )
        .into_response()
}

/// Live stream of stage activity for one run. Auth via ?token= (EventSource
/// cannot set headers). Emits each new stage log row as a JSON event; pings
/// while idle; ends when the run reaches a terminal state.
async fn sse_stage_logs(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
    Path(id): Path<String>,
) -> axum::response::Response {
    let authorized = match params.get("token") {
        Some(t) => matches!(state.manager.user_for_token(t), Ok(Some(_))),
        None => false,
    };
    if !authorized {
        return sse_unauthorized();
    }
    let db = state.manager.db().clone();
    let token = params.get("token").cloned().unwrap_or_default();
    let stream = async_stream::stream! {
        let mut after: i64 = 0;
        let mut idle_terminal_ticks = 0u32;
        loop {
            let allowed = db.user_for_token(&token).ok().flatten()
                .is_some_and(|user| db.can_access_run(&user.id, &id).unwrap_or(false));
            if !allowed {
                yield Ok(Event::default().event("auth").data("session or project access ended"));
                break;
            }
            let rows = db.list_stage_logs(&id, after, 500).unwrap_or_default();
            for r in rows {
                after = after.max(r.id);
                let data = serde_json::to_string(&r).unwrap_or_default();
                yield Ok::<Event, std::convert::Infallible>(Event::default().data(data));
            }
            let terminal = matches!(db.load_run(&id).ok().flatten().map(|r| r.status().to_string()), Some(s) if s == "completed" || s == "failed" || s == "cancelled" || s == "interrupted");
            if terminal {
                idle_terminal_ticks += 1;
                if idle_terminal_ticks >= 3 { break; }
            } else {
                idle_terminal_ticks = 0;
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    };

    Sse::new(Box::pin(stream) as EventStream)
        .keep_alive(
            axum::response::sse::KeepAlive::new()
                .interval(std::time::Duration::from_secs(15))
                .text("ping"),
        )
        .into_response()
}

async fn delete_run(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = require_auth(&state, &headers) {
        return r;
    }
    match state.manager.delete_run(&id).await {
        Ok(()) => Json(serde_json::json!({ "deleted": true, "id": id })).into_response(),
        Err(e) if e.contains("does not exist") => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": e })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": e })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct CancelInputBody {
    seq: u64,
}

async fn cancel_run_input(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<CancelInputBody>,
) -> impl IntoResponse {
    if let Err(r) = require_auth(&state, &headers) {
        return r;
    }
    match state.manager.cancel_web_input(&id, body.seq) {
        Ok(cancelled) => Json(serde_json::json!({ "cancelled": cancelled })).into_response(),
        Err(e) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": e })),
        )
            .into_response(),
    }
}

async fn set_secret(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
    Json(req): Json<SetSecretRequest>,
) -> impl IntoResponse {
    if let Err(r) = require_auth(&state, &headers) {
        return r;
    }
    tracing::debug!(project_id = %project_id, key = %req.key, "set_secret handler entered");

    let secrets_store = match state.secrets_store() {
        Some(ss) => ss,
        None => {
            tracing::warn!("Secrets store not configured, rejecting set_secret");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "Secrets store not configured" })),
            )
                .into_response();
        }
    };

    if let Err(e) = validate_secret_key(&req.key) {
        tracing::debug!(key = %req.key, error = %e, "Secret key validation failed");
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response();
    }

    if req.key.starts_with("__") {
        tracing::debug!(key = %req.key, "Rejected reserved key prefix in set_secret");
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "Reserved key prefix" })),
        )
            .into_response();
    }

    if let Err(e) = validate_secret_value(&req.value) {
        tracing::debug!(key = %req.key, error = %e, "Secret value validation failed");
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response();
    }

    if let Err(e) = secrets_store.set(&project_id, &req.key, &req.value).await {
        tracing::error!(project_id = %project_id, key = %req.key, error = %e, "Failed to set secret");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response();
    }

    tracing::debug!(project_id = %project_id, key = %req.key, "Secret set successfully");

    let _ = state.manager.sender().send(ManagerEvent::SecretUpdated {
        project_id: project_id.clone(),
        key: req.key.clone(),
    });

    (
        StatusCode::OK,
        Json(serde_json::json!({ "key": req.key, "status": "set" })),
    )
        .into_response()
}

async fn list_secrets(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = require_auth(&state, &headers) {
        return r;
    }
    tracing::debug!(project_id = %project_id, "list_secrets handler entered");

    let secrets_store = match state.secrets_store() {
        Some(ss) => ss,
        None => {
            tracing::warn!("Secrets store not configured, rejecting list_secrets");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "Secrets store not configured" })),
            )
                .into_response();
        }
    };

    let keys = match secrets_store.list(&project_id).await {
        Ok(k) => k,
        Err(e) => {
            tracing::error!(project_id = %project_id, error = %e, "Failed to list secrets");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };

    let mut entries: Vec<SecretEntry> = Vec::new();
    for key in &keys {
        if key.starts_with("__") {
            continue;
        }
        let masked_value = match secrets_store.get(&project_id, key).await {
            Ok(Some(val)) => mask_secret(&val),
            Ok(None) => "***".to_string(),
            Err(e) => {
                tracing::warn!(project_id = %project_id, key = %key, error = %e, "Failed to get secret for masking, skipping");
                continue;
            }
        };
        entries.push(SecretEntry {
            key: key.clone(),
            masked_value,
            updated_at: None,
        });
    }

    tracing::debug!(project_id = %project_id, count = entries.len(), "Listed secrets");
    (StatusCode::OK, Json(entries)).into_response()
}

async fn get_secret(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((project_id, key)): Path<(String, String)>,
) -> impl IntoResponse {
    if let Err(r) = require_auth(&state, &headers) {
        return r;
    }
    tracing::debug!(project_id = %project_id, key = %key, "get_secret handler entered");

    let secrets_store = match state.secrets_store() {
        Some(ss) => ss,
        None => {
            tracing::warn!("Secrets store not configured, rejecting get_secret");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "Secrets store not configured" })),
            )
                .into_response();
        }
    };

    if key.starts_with("__") {
        tracing::debug!(key = %key, "Rejected reserved key prefix in get_secret");
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "Reserved key prefix" })),
        )
            .into_response();
    }

    match secrets_store.get(&project_id, &key).await {
        Ok(Some(val)) => {
            tracing::debug!(project_id = %project_id, key = %key, "Secret found");
            let entry = SecretEntry {
                key,
                masked_value: mask_secret(&val),
                updated_at: None,
            };
            (StatusCode::OK, Json(entry)).into_response()
        }
        Ok(None) => {
            tracing::debug!(project_id = %project_id, key = %key, "Secret not found");
            (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "Secret not found" })),
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!(project_id = %project_id, key = %key, error = %e, "Failed to get secret");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    }
}

async fn delete_secret(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((project_id, key)): Path<(String, String)>,
) -> impl IntoResponse {
    if let Err(r) = require_auth(&state, &headers) {
        return r;
    }
    tracing::debug!(project_id = %project_id, key = %key, "delete_secret handler entered");

    let secrets_store = match state.secrets_store() {
        Some(ss) => ss,
        None => {
            tracing::warn!("Secrets store not configured, rejecting delete_secret");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "Secrets store not configured" })),
            )
                .into_response();
        }
    };

    if key.starts_with("__") {
        tracing::debug!(key = %key, "Rejected reserved key prefix in delete_secret");
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "Reserved key prefix" })),
        )
            .into_response();
    }

    if let Err(e) = secrets_store.delete(&project_id, &key).await {
        tracing::error!(project_id = %project_id, key = %key, error = %e, "Failed to delete secret");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response();
    }

    tracing::debug!(project_id = %project_id, key = %key, "Secret deleted");

    let _ = state.manager.sender().send(ManagerEvent::SecretUpdated {
        project_id: project_id.clone(),
        key: key.clone(),
    });

    (
        StatusCode::OK,
        Json(serde_json::json!({ "key": key, "deleted": true })),
    )
        .into_response()
}

async fn generate_ssh_key(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
    Json(req): Json<GenerateSshKeyRequest>,
) -> impl IntoResponse {
    let user = match require_auth(&state, &headers) {
        Ok(user) => user,
        Err(r) => return r,
    };
    if !state
        .manager
        .db()
        .can_access_project(&user.id, &project_id)
        .unwrap_or(false)
    {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "Project access denied" })),
        )
            .into_response();
    }
    tracing::debug!(project_id = %project_id, algorithm = %req.algorithm, "generate_ssh_key handler entered");

    let secrets_store = match state.secrets_store() {
        Some(ss) => ss,
        None => {
            tracing::warn!("Secrets store not configured, rejecting generate_ssh_key");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "Secrets store not configured" })),
            )
                .into_response();
        }
    };

    // Validate algorithm
    let algorithm = req.algorithm.to_lowercase();
    if !["ed25519", "rsa-4096", "ecdsa-256", "ecdsa-384", "ecdsa-521"].contains(&algorithm.as_str())
    {
        tracing::debug!(algorithm = %req.algorithm, "Unsupported SSH key algorithm");
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": format!("Unsupported algorithm: {}. Supported: ed25519, rsa-4096, ecdsa-256, ecdsa-384, ecdsa-521", req.algorithm) })),
        ).into_response();
    }

    // Delete old keys if they exist
    let _ = secrets_store.delete(&project_id, "__ssh_private_key").await;
    let _ = secrets_store.delete(&project_id, "__ssh_public_key").await;
    let _ = secrets_store.delete(&project_id, "__ssh_algorithm").await;

    // Generate key pair
    let (private_key_pem, public_key_openssh) = match generate_key_pair(&algorithm, &project_id) {
        Ok(pair) => pair,
        Err(e) => {
            tracing::error!(project_id = %project_id, algorithm = %algorithm, error = %e, "Failed to generate SSH key pair");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to generate SSH key: {}", e) })),
            )
                .into_response();
        }
    };

    // Store keys
    if let Err(e) = secrets_store
        .set(&project_id, "__ssh_private_key", &private_key_pem)
        .await
    {
        tracing::error!(project_id = %project_id, error = %e, "Failed to store SSH private key");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "Failed to store SSH key" })),
        )
            .into_response();
    }
    if let Err(e) = secrets_store
        .set(&project_id, "__ssh_public_key", &public_key_openssh)
        .await
    {
        tracing::error!(project_id = %project_id, error = %e, "Failed to store SSH public key");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "Failed to store SSH key" })),
        )
            .into_response();
    }
    if let Err(e) = secrets_store
        .set(&project_id, "__ssh_algorithm", &algorithm)
        .await
    {
        tracing::error!(project_id = %project_id, error = %e, "Failed to store SSH algorithm");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "Failed to store SSH key" })),
        )
            .into_response();
    }

    tracing::debug!(project_id = %project_id, algorithm = %algorithm, "SSH key pair generated and stored");

    let _ = state.manager.sender().send(ManagerEvent::SecretUpdated {
        project_id: project_id.clone(),
        key: "__ssh_key".to_string(),
    });

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "public_key": public_key_openssh,
            "algorithm": algorithm,
            "project_id": project_id,
        })),
    )
        .into_response()
}

fn generate_key_pair(algorithm: &str, _project_id: &str) -> anyhow::Result<(String, String)> {
    use ssh_key::{Algorithm, PrivateKey, rand_core::OsRng};

    let private_key = match algorithm {
        "ed25519" => PrivateKey::random(&mut OsRng, Algorithm::Ed25519)?,
        "rsa-4096" => PrivateKey::random(
            &mut OsRng,
            Algorithm::Rsa {
                hash: Some(ssh_key::HashAlg::Sha256),
            },
        )?,
        "ecdsa-256" => PrivateKey::random(
            &mut OsRng,
            Algorithm::Ecdsa {
                curve: ssh_key::EcdsaCurve::NistP256,
            },
        )?,
        "ecdsa-384" => PrivateKey::random(
            &mut OsRng,
            Algorithm::Ecdsa {
                curve: ssh_key::EcdsaCurve::NistP384,
            },
        )?,
        "ecdsa-521" => PrivateKey::random(
            &mut OsRng,
            Algorithm::Ecdsa {
                curve: ssh_key::EcdsaCurve::NistP521,
            },
        )?,
        _ => return Err(anyhow::anyhow!("Unsupported algorithm: {}", algorithm)),
    };

    let private_key_pem = private_key.to_openssh(ssh_key::LineEnding::LF)?.to_string();

    let public_key = private_key.public_key();
    let public_key_openssh = public_key.to_openssh()?.to_string();

    Ok((private_key_pem, public_key_openssh))
}

async fn get_ssh_key(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
) -> impl IntoResponse {
    let user = match require_auth(&state, &headers) {
        Ok(user) => user,
        Err(r) => return r,
    };
    if !state
        .manager
        .db()
        .can_access_project(&user.id, &project_id)
        .unwrap_or(false)
    {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "Project access denied" })),
        )
            .into_response();
    }
    tracing::debug!(project_id = %project_id, "get_ssh_key handler entered");

    let secrets_store = match state.secrets_store() {
        Some(ss) => ss,
        None => {
            tracing::warn!("Secrets store not configured, rejecting get_ssh_key");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "Secrets store not configured" })),
            )
                .into_response();
        }
    };

    let public_key = match secrets_store.get(&project_id, "__ssh_public_key").await {
        Ok(Some(val)) => val,
        Ok(None) => {
            tracing::debug!(project_id = %project_id, "SSH public key not found");
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "SSH key not found" })),
            )
                .into_response();
        }
        Err(e) => {
            tracing::error!(project_id = %project_id, error = %e, "Failed to get SSH public key");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };

    let algorithm = secrets_store
        .get(&project_id, "__ssh_algorithm")
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| infer_algorithm_from_key(&public_key));

    tracing::debug!(project_id = %project_id, algorithm = %algorithm, "SSH public key retrieved");

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "public_key": public_key,
            "algorithm": algorithm,
            "project_id": project_id,
        })),
    )
        .into_response()
}

fn infer_algorithm_from_key(public_key: &str) -> String {
    if public_key.starts_with("ssh-ed25519") {
        "ed25519".to_string()
    } else if public_key.starts_with("ssh-rsa") {
        "rsa-4096".to_string()
    } else if public_key.starts_with("ecdsa-sha2-nistp256") {
        "ecdsa-256".to_string()
    } else if public_key.starts_with("ecdsa-sha2-nistp384") {
        "ecdsa-384".to_string()
    } else if public_key.starts_with("ecdsa-sha2-nistp521") {
        "ecdsa-521".to_string()
    } else {
        "unknown".to_string()
    }
}

async fn delete_ssh_key(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
) -> impl IntoResponse {
    let user = match require_auth(&state, &headers) {
        Ok(user) => user,
        Err(r) => return r,
    };
    if !state
        .manager
        .db()
        .can_access_project(&user.id, &project_id)
        .unwrap_or(false)
    {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "Project access denied" })),
        )
            .into_response();
    }
    tracing::debug!(project_id = %project_id, "delete_ssh_key handler entered");

    let secrets_store = match state.secrets_store() {
        Some(ss) => ss,
        None => {
            tracing::warn!("Secrets store not configured, rejecting delete_ssh_key");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "Secrets store not configured" })),
            )
                .into_response();
        }
    };

    let _ = secrets_store.delete(&project_id, "__ssh_private_key").await;
    let _ = secrets_store.delete(&project_id, "__ssh_public_key").await;
    let _ = secrets_store.delete(&project_id, "__ssh_algorithm").await;

    tracing::debug!(project_id = %project_id, "SSH key pair deleted");

    let _ = state.manager.sender().send(ManagerEvent::SecretUpdated {
        project_id: project_id.clone(),
        key: "__ssh_key".to_string(),
    });

    (
        StatusCode::OK,
        Json(serde_json::json!({ "deleted": true, "project_id": project_id })),
    )
        .into_response()
}

#[derive(Deserialize)]
struct SshKnownHostsBody {
    known_hosts: String,
}

async fn set_ssh_known_hosts(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
    Json(body): Json<SshKnownHostsBody>,
) -> impl IntoResponse {
    let user = match require_auth(&state, &headers) {
        Ok(user) => user,
        Err(r) => return r,
    };
    if !state
        .manager
        .db()
        .can_access_project(&user.id, &project_id)
        .unwrap_or(false)
    {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "Project access denied" })),
        )
            .into_response();
    }
    if let Err(error) = crate::ssh_validation::validate_known_hosts(&body.known_hosts) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!("known_hosts must contain verified host/key lines: {error}")
            })),
        )
            .into_response();
    }
    let Some(secrets) = state.secrets_store() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "Secrets store not configured" })),
        )
            .into_response();
    };
    if let Err(error) = secrets
        .set(&project_id, "__ssh_known_hosts", &body.known_hosts)
        .await
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": error.to_string() })),
        )
            .into_response();
    }
    let _ = state.manager.sender().send(ManagerEvent::SecretUpdated {
        project_id: project_id.clone(),
        key: "__ssh_known_hosts".into(),
    });
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "project_id": project_id, "configured": true,
            "verification": "strict host key checking with this file only"
        })),
    )
        .into_response()
}

async fn get_ssh_known_hosts(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
) -> impl IntoResponse {
    let user = match require_auth(&state, &headers) {
        Ok(user) => user,
        Err(r) => return r,
    };
    if !state
        .manager
        .db()
        .can_access_project(&user.id, &project_id)
        .unwrap_or(false)
    {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "Project access denied" })),
        )
            .into_response();
    }
    let Some(secrets) = state.secrets_store() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "Secrets store not configured" })),
        )
            .into_response();
    };
    match secrets.get(&project_id, "__ssh_known_hosts").await {
        Ok(Some(known_hosts)) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "project_id": project_id, "configured": true, "known_hosts": known_hosts
            })),
        )
            .into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "project_id": project_id, "configured": false,
                "error": "No verified host identity configured"
            })),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

async fn delete_ssh_known_hosts(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
) -> impl IntoResponse {
    let user = match require_auth(&state, &headers) {
        Ok(user) => user,
        Err(r) => return r,
    };
    if !state
        .manager
        .db()
        .can_access_project(&user.id, &project_id)
        .unwrap_or(false)
    {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "Project access denied" })),
        )
            .into_response();
    }
    let Some(secrets) = state.secrets_store() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "Secrets store not configured" })),
        )
            .into_response();
    };
    if let Err(error) = secrets.delete(&project_id, "__ssh_known_hosts").await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": error.to_string() })),
        )
            .into_response();
    }
    let _ = state.manager.sender().send(ManagerEvent::SecretUpdated {
        project_id: project_id.clone(),
        key: "__ssh_known_hosts".into(),
    });
    (
        StatusCode::OK,
        Json(serde_json::json!({ "deleted": true, "project_id": project_id })),
    )
        .into_response()
}

async fn reprovision_project(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
) -> impl IntoResponse {
    let user = match require_auth(&state, &headers) {
        Ok(user) => user,
        Err(r) => return r,
    };
    if !state
        .manager
        .db()
        .can_access_project(&user.id, &project_id)
        .unwrap_or(false)
    {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "Project access denied" })),
        )
            .into_response();
    }
    match state.manager.reprovision_project(&project_id).await {
        Ok(project) => (
            StatusCode::OK,
            Json(serde_json::json!({ "project": project })),
        )
            .into_response(),
        Err(error) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

async fn get_run_publication(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let user = match require_auth(&state, &headers) {
        Ok(user) => user,
        Err(r) => return r,
    };
    if !state
        .manager
        .db()
        .can_access_run(&user.id, &id)
        .unwrap_or(false)
    {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "Project access denied" })),
        )
            .into_response();
    }
    match state.manager.publications(&id) {
        Ok(publications) => {
            let latest = publications.last().cloned();
            Json(serde_json::json!({
                "run_id": id,
                "publication": latest,
                "history": publications,
                "draft_pr": latest.as_ref().and_then(|publication| publication.pr_url.clone()),
                "reconciliation_required": latest.as_ref().is_some_and(|publication| {
                    !matches!(publication.push_state, tj_db::publications::PublicationState::Succeeded)
                        || !matches!(publication.pr_state, tj_db::publications::PublicationState::Succeeded)
                }),
            })).into_response()
        }
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}
