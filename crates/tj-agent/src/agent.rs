use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tj_chat::{ChatMessage, ChatProvider, ChatSession, ConversationContext, ConversationType};
use tj_provider::types::{ChatRequest, Message, Role, ToolCall, ToolDef, ToolFunction};

// ---------------------------------------------------------------------------
// SystemOperations — async trait defined in consumer crate (dependency inversion)
// Manager and Chief both depend on this abstraction.
// ---------------------------------------------------------------------------

pub trait SystemOperations: Send + Sync {
    fn get_job<'a>(
        &'a self,
        id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Option<serde_json::Value>> + Send + 'a>>;
    fn list_jobs<'a>(&'a self)
    -> Pin<Box<dyn Future<Output = Vec<serde_json::Value>> + Send + 'a>>;
    fn create_job<'a>(
        &'a self,
        description: &'a str,
        project_id: Option<&'a str>,
        lane_id: Option<&'a str>,
        repo_url: Option<&'a str>,
        base_branch: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Option<serde_json::Value>> + Send + 'a>>;
    fn cancel_job<'a>(
        &'a self,
        job_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;
    fn list_flows<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Vec<serde_json::Value>> + Send + 'a>>;
    fn approve_gate<'a>(
        &'a self,
        job_id: &'a str,
        gate_instance_id: &'a str,
        expected_state_version: i64,
        idempotency_key: &'a str,
        approved: bool,
        reason: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, String>> + Send + 'a>>;
    fn create_project<'a>(
        &'a self,
        name: &'a str,
        repo_url: Option<&'a str>,
        base_branch: Option<&'a str>,
        git_provider_json: Option<&'a str>,
        workflow_id: Option<&'a str>,
        catalog_id: Option<&'a str>,
        legacy_profile: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Option<serde_json::Value>> + Send + 'a>>;
    fn list_projects<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Vec<serde_json::Value>> + Send + 'a>>;
    /// Inspect durable external delivery state, provenance and rejected
    /// callbacks. This is read-only: Chief cannot dispatch, authorize, merge or
    /// release. Those actions require the authenticated API/UI surface.
    fn get_delivery<'a>(
        &'a self,
        job_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Option<serde_json::Value>> + Send + 'a>>;
    fn list_lanes<'a>(
        &'a self,
        project_id: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Vec<serde_json::Value>> + Send + 'a>>;
}

// ---------------------------------------------------------------------------
// Async tool executor — shared between Agent and Chief via SystemOperations.
// ---------------------------------------------------------------------------

type ToolExecutor =
    for<'a> fn(
        &'a ToolCall,
        &'a dyn SystemOperations,
    ) -> Pin<Box<dyn Future<Output = Option<serde_json::Value>> + Send + 'a>>;

struct ToolEntry {
    definition: ToolDef,
    executor: ToolExecutor,
}

fn tool_registry() -> HashMap<&'static str, ToolEntry> {
    let mut m: HashMap<&'static str, ToolEntry> = HashMap::new();

    m.insert("list_flows", ToolEntry {
        definition: ToolDef {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "list_flows".into(),
                description: "List registered workflows (Flows/Lanes) that can be started as Jobs, with catalog metadata, maturity, stages, gate presence, declared workspace/capabilities and lane IDs. Pass a lane id to start_job. Workflow availability comes from the registry; there is no built-in default.".into(),
                parameters: serde_json::json!({ "type": "object", "properties": {} }),
            },
        },
        executor: |_call, ops| {
            Box::pin(async move {
                let flows = ops.list_flows().await;
                Some(serde_json::json!({ "flows": flows }))
            })
        },
    });

    m.insert("start_job", ToolEntry {
        definition: ToolDef {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "start_job".into(),
                description: "Start a new Job (pipeline execution). Requires a description and a project — every run executes on a workflow (project + lane).".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "description": { "type": "string", "description": "Description of what the Job should accomplish" },
                        "project_id": { "type": "string", "description": "Project ID to run against (required)" },
                        "lane_id": { "type": "string", "description": "Lane ID (workflow). If omitted, uses the project's Default lane." },
                        "repo_url": { "type": "string", "description": "Optional git repository URL (overrides project setting)" },
                        "base_branch": { "type": "string", "description": "Optional base branch (overrides project setting)" }
                    },
                    "required": ["description", "project_id"]
                }),
            },
        },
        executor: |call, ops| {
            Box::pin(async move {
                let description = call.arguments.get("description")?.as_str()?;
                let project_id = call.arguments.get("project_id").and_then(|v| v.as_str());
                let lane_id = call.arguments.get("lane_id").and_then(|v| v.as_str());
                let repo_url = call.arguments.get("repo_url").and_then(|v| v.as_str());
                let base_branch = call.arguments.get("base_branch").and_then(|v| v.as_str());
                ops.create_job(description, project_id, lane_id, repo_url, base_branch).await
            })
        },
    });

    m.insert("list_jobs", ToolEntry {
        definition: ToolDef {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "list_jobs".into(),
                description: "List all Jobs (current and past) with their status, current Flow, and current Stage.".into(),
                parameters: serde_json::json!({ "type": "object", "properties": {} }),
            },
        },
        executor: |_call, ops| {
            Box::pin(async move {
                let jobs = ops.list_jobs().await;
                Some(serde_json::json!({ "jobs": jobs }))
            })
        },
    });

    m.insert("get_job", ToolEntry {
        definition: ToolDef {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "get_job".into(),
                description: "Get detailed status of a specific Job by its ID, including which Flow and Stage it is currently at.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "job_id": { "type": "string", "description": "The Job ID" }
                    },
                    "required": ["job_id"]
                }),
            },
        },
        executor: |call, ops| {
            Box::pin(async move {
                let job_id = call.arguments.get("job_id")?.as_str()?;
                ops.get_job(job_id).await
            })
        },
    });

    m.insert("approve_gate", ToolEntry {
        definition: ToolDef {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "approve_gate".into(),
                description: "Approve or reject the exact active Gate on a Job. Call get_job first, then copy its gate_instance_id and state_version and provide a fresh idempotency_key. Stale gates conflict; never guess these values.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "job_id": { "type": "string", "description": "The Job ID waiting at a Gate" },
                        "gate_instance_id": { "type": "string", "description": "Exact active gate_instance_id returned by get_job" },
                        "expected_state_version": { "type": "integer", "description": "Exact state_version returned by get_job" },
                        "idempotency_key": { "type": "string", "description": "Unique client-generated key reused only for retries of this exact decision" },
                        "approved": { "type": "boolean", "description": "Whether to approve (true) or reject (false)" },
                        "reason": { "type": "string", "description": "Optional reason for the decision" }
                    },
                    "required": ["job_id", "gate_instance_id", "expected_state_version", "idempotency_key", "approved"]
                }),
            },
        },
        executor: |call, ops| {
            Box::pin(async move {
                let job_id = call.arguments.get("job_id")?.as_str()?;
                let gate_instance_id = call.arguments.get("gate_instance_id")?.as_str()?;
                let expected_state_version = call.arguments.get("expected_state_version")?.as_i64()?;
                let idempotency_key = call.arguments.get("idempotency_key")?.as_str()?;
                let approved = call.arguments.get("approved")?.as_bool()?;
                let reason = call.arguments.get("reason").and_then(|v| v.as_str());
                match ops.approve_gate(job_id, gate_instance_id, expected_state_version, idempotency_key, approved, reason).await {
                    Ok(receipt) => Some(receipt),
                    Err(error) => Some(serde_json::json!({ "error": error, "job_id": job_id })),
                }
            })
        },
    });

    m.insert(
        "cancel_job",
        ToolEntry {
            definition: ToolDef {
                tool_type: "function".into(),
                function: ToolFunction {
                    name: "cancel_job".into(),
                    description: "Cancel a running Job.".into(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "job_id": { "type": "string", "description": "The Job ID to cancel" }
                        },
                        "required": ["job_id"]
                    }),
                },
            },
            executor: |call, ops| {
                Box::pin(async move {
                    let job_id = call.arguments.get("job_id")?.as_str()?;
                    match ops.cancel_job(job_id).await {
                        Ok(()) => {
                            Some(serde_json::json!({ "status": "cancelled", "job_id": job_id }))
                        }
                        Err(error) => Some(serde_json::json!({ "error": error, "job_id": job_id })),
                    }
                })
            },
        },
    );

    m.insert("create_project", ToolEntry {
        definition: ToolDef {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "create_project".into(),
                description: "Create a new Project from a workflow in the registry. Use list_flows first, then pass either workflow_id (preferred) or catalog_id. If exactly one catalog is marked default, both may be omitted. A repository URL is accepted only when the registered workflow declares a repository workspace; otherwise it is refused. A Project has a name, optional repo URL, base branch, and optional git provider config (JSON). Projects contain Lanes (workflow instances).".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Project name" },
                        "repo_url": { "type": "string", "description": "Optional git repository URL, only for a workflow that declares a repository workspace" },
                        "base_branch": { "type": "string", "description": "Optional base branch (default: main)" },
                        "git_provider_json": { "type": "string", "description": "Optional git provider configuration as a JSON string" },
                        "workflow_id": { "type": "string", "description": "Exact workflow version id from the registry (preferred)" },
                        "catalog_id": { "type": "string", "description": "Registered catalog id; selects its highest imported revision" },
                        "profile": { "type": "string", "description": "Legacy alias, accepted only when it names a registered catalog id" }
                    },
                    "required": ["name"]
                }),
            },
        },
        executor: |call, ops| {
            Box::pin(async move {
                let name = call.arguments.get("name")?.as_str()?;
                let repo_url = call.arguments.get("repo_url").and_then(|v| v.as_str());
                let base_branch = call.arguments.get("base_branch").and_then(|v| v.as_str());
                let git_provider_json = call.arguments.get("git_provider_json").and_then(|v| v.as_str());
                let workflow_id = call.arguments.get("workflow_id").and_then(|v| v.as_str());
                let catalog_id = call.arguments.get("catalog_id").and_then(|v| v.as_str());
                let legacy_profile = call.arguments.get("profile").and_then(|v| v.as_str());
                ops.create_project(name, repo_url, base_branch, git_provider_json, workflow_id, catalog_id, legacy_profile).await
            })
        },
    });

    m.insert(
        "list_projects",
        ToolEntry {
            definition: ToolDef {
                tool_type: "function".into(),
                function: ToolFunction {
                    name: "list_projects".into(),
                    description: "List all Projects.".into(),
                    parameters: serde_json::json!({ "type": "object", "properties": {} }),
                },
            },
            executor: |_call, ops| {
                Box::pin(async move {
                    let projects = ops.list_projects().await;
                    Some(serde_json::json!({ "projects": projects }))
                })
            },
        },
    );

    m.insert("get_delivery", ToolEntry {
        definition: ToolDef {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "get_delivery".into(),
                description: "Inspect durable external CI/deployment state for a Job: exact packet/plan/task/PR/commit identities, dispatch/CI/deployment/verification/observation states, release recommendation, immutable production decisions and rejected callback errors. Read-only; Chief cannot deploy, authorize production, merge or roll back.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "job_id": { "type": "string", "description": "The Job ID" }
                    },
                    "required": ["job_id"]
                }),
            },
        },
        executor: |call, ops| {
            Box::pin(async move {
                let job_id = call.arguments.get("job_id")?.as_str()?;
                match ops.get_delivery(job_id).await {
                    Some(delivery) => Some(delivery),
                    None => Some(serde_json::json!({
                        "error": "Job is missing, inaccessible, or has no delivery history",
                        "job_id": job_id
                    })),
                }
            })
        },
    });

    m.insert("list_lanes", ToolEntry {
        definition: ToolDef {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "list_lanes".into(),
                description: "List all Lanes (pipeline templates). Optionally filter by project_id.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "project_id": { "type": "string", "description": "Optional project ID to filter lanes" }
                    }
                }),
            },
        },
        executor: |call, ops| {
            Box::pin(async move {
                let project_id = call.arguments.get("project_id").and_then(|v| v.as_str());
                let lanes = ops.list_lanes(project_id).await;
                Some(serde_json::json!({ "lanes": lanes }))
            })
        },
    });

    m
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSession {
    pub id: String,
    pub job_id: Option<String>,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentResponse {
    Text { content: String },
    ToolCalls { calls: Vec<ToolCall> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PipelineEvent {
    FlowCompleted {
        run_id: String,
        flow: String,
        summary: String,
    },
    GateReached {
        run_id: String,
        flow: String,
        summary: String,
    },
    StageCompleted {
        run_id: String,
        stage: String,
        summary: String,
    },
    StageWaitingForInput {
        run_id: String,
        stage: String,
        message: String,
    },
    PipelineCompleted {
        run_id: String,
        summary: String,
    },
    PipelineFailed {
        run_id: String,
        reason: String,
        user_message: Option<String>,
    },
}

impl PipelineEvent {
    pub fn run_id(&self) -> &str {
        match self {
            PipelineEvent::FlowCompleted { run_id, .. } => run_id,
            PipelineEvent::GateReached { run_id, .. } => run_id,
            PipelineEvent::StageCompleted { run_id, .. } => run_id,
            PipelineEvent::StageWaitingForInput { run_id, .. } => run_id,
            PipelineEvent::PipelineCompleted { run_id, .. } => run_id,
            PipelineEvent::PipelineFailed { run_id, .. } => run_id,
        }
    }
}

pub struct Agent {
    id: String,
    model_name: String,
    provider: Box<dyn tj_provider::LlmProvider>,
    model: String,
    system_prompt: String,
    max_tool_rounds: u32,
    tools: Vec<ToolDef>,
    tool_names: HashSet<String>,
    executors: HashMap<String, ToolExecutor>,
}

impl Agent {
    pub fn new(
        id: String,
        provider: Box<dyn tj_provider::LlmProvider>,
        model: String,
        system_prompt: String,
        enabled_tools: &[String],
    ) -> Self {
        let registry = tool_registry();
        let mut tools = Vec::new();
        let mut tool_names = HashSet::new();
        let mut executors = HashMap::new();

        for tool_name in enabled_tools {
            if let Some(entry) = registry.get(tool_name.as_str()) {
                tools.push(entry.definition.clone());
                tool_names.insert(tool_name.clone());
                executors.insert(tool_name.clone(), entry.executor);
            } else {
                tracing::warn!(tool = %tool_name, "Unknown tool in agent config, skipping");
            }
        }

        let model_name = format!("turbojet-{}", id);

        Self {
            id,
            model_name,
            provider,
            model,
            system_prompt,
            max_tool_rounds: 5,
            tools,
            tool_names,
            executors,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn model_name(&self) -> &str {
        &self.model_name
    }
    pub fn tool_names(&self) -> &HashSet<String> {
        &self.tool_names
    }

    pub fn with_max_tool_rounds(mut self, rounds: u32) -> Self {
        self.max_tool_rounds = rounds;
        self
    }

    pub fn create_session(&self, session_id: String) -> AgentSession {
        AgentSession {
            id: session_id,
            job_id: None,
            messages: Vec::new(),
        }
    }

    pub fn inject_client_tool_result(
        &self,
        session: &mut AgentSession,
        tool_call_id: &str,
        result: String,
    ) {
        session.messages.push(Message {
            role: Role::Tool,
            content: format!(
                "{{\"tool_call_id\": \"{}\", \"result\": {}}}",
                tool_call_id, result
            ),
        });
    }

    pub async fn handle_message(
        &self,
        session: &mut AgentSession,
        user_msg: &str,
        client_tools: Vec<ToolDef>,
        ctx: &dyn SystemOperations,
    ) -> Result<AgentResponse> {
        session.messages.push(Message {
            role: Role::User,
            content: user_msg.into(),
        });

        tracing::info!(agent = %self.id, "Received user message: {}", user_msg.chars().take(100).collect::<String>());

        let all_tools: Vec<ToolDef> = self
            .tools
            .iter()
            .chain(client_tools.iter())
            .cloned()
            .collect();

        let all_names: Vec<&str> = all_tools.iter().map(|t| t.function.name.as_str()).collect();
        tracing::debug!(agent = %self.id, tools = ?all_names, "Agent tools available");

        let mut rounds = 0;

        loop {
            let mut messages = vec![Message {
                role: Role::System,
                content: self.build_system_context(session),
            }];
            messages.extend(session.messages.clone());

            tracing::debug!(agent = %self.id, round = rounds, "Sending to LLM");

            let request = ChatRequest {
                model: self.model.clone(),
                messages,
                tools: Some(all_tools.clone()),
                max_tokens: Some(2048),
                temperature: None,
            };

            let response = self.provider.chat(request).await?;

            if response.content.is_empty() {
                tracing::debug!("LLM returned tool calls (no text content)");
            } else {
                tracing::info!(agent = %self.id, "Responded: {}", response.content.chars().take(150).collect::<String>());
            }

            match &response.tool_calls {
                Some(calls) if !calls.is_empty() => {
                    let (agent_calls, client_calls): (Vec<_>, Vec<_>) = calls
                        .iter()
                        .partition(|c| self.tool_names.contains(&c.name));

                    if !response.content.is_empty() {
                        tracing::debug!(
                            "LLM returned both content and tool_calls, discarding content"
                        );
                    }

                    if !client_calls.is_empty() {
                        return Ok(AgentResponse::ToolCalls {
                            calls: client_calls.into_iter().cloned().collect(),
                        });
                    }

                    if rounds >= self.max_tool_rounds {
                        let content = if response.content.is_empty() {
                            "I've completed the requested operations.".into()
                        } else {
                            response.content.clone()
                        };
                        session.messages.push(Message {
                            role: Role::Assistant,
                            content: content.clone(),
                        });
                        return Ok(AgentResponse::Text { content });
                    }

                    rounds += 1;

                    for call in &agent_calls {
                        tracing::info!(agent = %self.id, tool = %call.name, "Calling tool");
                        let result = self.execute_tool(call, ctx).await;
                        let result_val =
                            result.unwrap_or(serde_json::json!({"error": "tool execution failed"}));
                        tracing::info!(agent = %self.id, tool = %call.name, "Tool result: {}", serde_json::to_string(&result_val).unwrap_or_default().chars().take(200).collect::<String>());
                        let result_str = serde_json::to_string(&result_val)
                            .unwrap_or_else(|_| "{\"error\": \"serialization failed\"}".into());
                        session.messages.push(Message {
                            role: Role::Tool,
                            content: result_str,
                        });
                    }
                }
                _ => {
                    session.messages.push(Message {
                        role: Role::Assistant,
                        content: response.content.clone(),
                    });
                    return Ok(AgentResponse::Text {
                        content: response.content,
                    });
                }
            }
        }
    }

    async fn execute_tool(
        &self,
        call: &ToolCall,
        ctx: &dyn SystemOperations,
    ) -> Option<serde_json::Value> {
        if let Some(executor) = self.executors.get(&call.name) {
            executor(call, ctx).await
        } else {
            tracing::warn!(tool = %call.name, agent = %self.id, "Unknown tool called");
            Some(serde_json::json!({ "error": format!("Unknown tool: {}", call.name) }))
        }
    }

    pub fn inject_event(&self, session: &mut AgentSession, event: &PipelineEvent) {
        let message = match event {
            PipelineEvent::FlowCompleted {
                run_id,
                flow,
                summary,
            } => {
                session.job_id = Some(run_id.clone());
                format!(
                    "**Flow completed** '{}' for Job {}: {}",
                    flow, run_id, summary
                )
            }
            PipelineEvent::GateReached {
                run_id,
                flow,
                summary,
            } => {
                session.job_id = Some(run_id.clone());
                format!(
                    "**Gate reached** at Flow '{}' for Job {}: {}",
                    flow, run_id, summary
                )
            }
            PipelineEvent::StageCompleted {
                run_id,
                stage,
                summary,
            } => {
                session.job_id = Some(run_id.clone());
                format!(
                    "Stage '{}' completed for Job {}: {}",
                    stage, run_id, summary
                )
            }
            PipelineEvent::StageWaitingForInput {
                run_id,
                stage,
                message: msg,
            } => {
                session.job_id = Some(run_id.clone());
                format!(
                    "Stage '{}' waiting for input for Job {}: {}",
                    stage, run_id, msg
                )
            }
            PipelineEvent::PipelineCompleted { run_id, summary } => {
                session.job_id = None;
                format!("Job {} completed: {}", run_id, summary)
            }
            PipelineEvent::PipelineFailed {
                run_id,
                reason,
                user_message,
            } => {
                session.job_id = None;
                // Use the user-friendly message if available, otherwise fall back to technical reason
                user_message
                    .clone()
                    .unwrap_or_else(|| format!("Job {} failed: {}", run_id, reason))
            }
        };

        session.messages.push(Message {
            role: Role::System,
            content: message,
        });
    }

    fn build_system_context(&self, session: &AgentSession) -> String {
        let mut ctx = self.system_prompt.clone();

        if let Some(job_id) = &session.job_id {
            ctx.push_str(&format!("\n\nCurrent active Job: {}", job_id));
        }

        ctx.push_str("\n\nYou have access to pipeline management tools. Use them when the user asks about Job status, wants to start Jobs, approve Gates, cancel Jobs, list Flows, or signal merge completion. Do NOT mention tools to the user — just use them naturally. IMPORTANT: When you call a tool, return ONLY the tool call with NO text content. When you respond with text, do NOT include any tool calls. Always choose one: text response OR tool calls, never both.");

        ctx
    }
}

// ---------------------------------------------------------------------------
// Chief — peer of Manager, holds Arc<dyn SystemOperations>
// Owns its DM ChatSession and LLM provider. Runs its own event loop.
// ---------------------------------------------------------------------------

pub struct ChiefConfig {
    pub agent_id: String,
    pub provider: Box<dyn tj_provider::LlmProvider>,
    pub model: String,
    pub system_prompt: String,
    pub enabled_tools: Vec<String>,
}

pub struct Chief {
    system_ops: Arc<dyn SystemOperations>,
    provider: Box<dyn tj_provider::LlmProvider>,
    model: String,
    system_prompt: String,
    max_tool_rounds: u32,
    tools: Vec<ToolDef>,
    #[allow(dead_code)]
    tool_names: HashSet<String>,
    executors: HashMap<String, ToolExecutor>,
}

impl Chief {
    pub fn new(config: ChiefConfig, system_ops: Arc<dyn SystemOperations>) -> Self {
        let registry = tool_registry();
        let mut tools = Vec::new();
        let mut tool_names = HashSet::new();
        let mut executors = HashMap::new();

        for tool_name in &config.enabled_tools {
            if let Some(entry) = registry.get(tool_name.as_str()) {
                tools.push(entry.definition.clone());
                tool_names.insert(tool_name.clone());
                executors.insert(tool_name.clone(), entry.executor);
            } else {
                tracing::warn!(tool = %tool_name, "Unknown tool in chief config, skipping");
            }
        }

        Self {
            system_ops,
            provider: config.provider,
            model: config.model,
            system_prompt: config.system_prompt,
            max_tool_rounds: 5,
            tools,
            tool_names,
            executors,
        }
    }

    /// Run the Chief event loop. Listens for DMs and responds via LLM + SystemOperations.
    pub async fn run(&self, chat_provider: Arc<dyn ChatProvider>) {
        tracing::info!("Chief starting event loop");

        let ctx = ConversationContext {
            project_id: String::new(),
            project_slug: "turbojet".into(),
            lane_id: String::new(),
            lane_slug: String::new(),
            run_id: String::new(),
            flow_name: None,
            flow_number: None,
            run_description: None,
            stage_name: None,
            stage_execution_id: None,
        };

        let conv = match chat_provider
            .create_conversation(&ctx, ConversationType::DirectMessage)
            .await
        {
            Ok(c) => {
                tracing::info!(provider_key = %c.provider_key, "Chief DM conversation created");
                c
            }
            Err(e) => {
                tracing::error!(error = %e, "Chief failed to create DM conversation");
                return;
            }
        };

        // Create a DM ChatSession for bidirectional communication
        let session: Arc<dyn ChatSession> = match chat_provider
            .create_dm_session(&conv.provider_key)
            .await
        {
            Ok(s) => {
                tracing::info!("Chief DM session created");
                s
            }
            Err(e) => {
                tracing::error!(error = %e, "Chief failed to create DM session, falling back to listen/send pattern");
                // Fall back to the legacy listen+send pattern
                self.run_legacy(chat_provider, &conv).await;
                return;
            }
        };

        // Primary event loop: receive -> LLM -> system_ops -> send
        loop {
            tracing::debug!("Chief waiting for incoming message");
            let incoming = match session.receive().await {
                Ok(msg) => msg,
                Err(e) => {
                    tracing::error!(error = %e, "Chief session receive error");
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    continue;
                }
            };

            tracing::info!(content_len = incoming.len(), "Chief received DM");

            let response = match self.handle_dm(&incoming).await {
                Ok(text) => text,
                Err(e) => {
                    tracing::error!(error = %e, "Chief LLM error");
                    format!("Sorry, I encountered an error: {}", e)
                }
            };

            if let Err(e) = session.send(&response).await {
                tracing::error!(error = %e, "Chief failed to send response");
            }
        }
    }

    /// Legacy listen+send pattern using ChatProvider directly (for providers without ChatSession).
    async fn run_legacy(
        &self,
        chat_provider: Arc<dyn ChatProvider>,
        conv: &tj_chat::ConversationId,
    ) {
        use futures::StreamExt;

        tracing::info!("Chief running in legacy listen/send mode");

        loop {
            let mut stream = chat_provider.listen(conv);
            while let Some(result) = StreamExt::next(&mut stream).await {
                match result {
                    Ok(msg) => {
                        tracing::info!(sender = %msg.sender, content = %msg.content.chars().take(100).collect::<String>(), "Chief DM received (legacy)");

                        let response = match self.handle_dm(&msg.content).await {
                            Ok(text) => text,
                            Err(e) => {
                                tracing::error!(error = %e, "Chief LLM error (legacy)");
                                format!("Sorry, I encountered an error: {}", e)
                            }
                        };

                        let reply = ChatMessage {
                            sender: "TurboJet".into(),
                            sender_id: msg.sender_id.clone(),
                            content: response,
                            timestamp: chrono::Utc::now(),
                        };
                        if let Err(e) = chat_provider.send_message(conv, &reply).await {
                            tracing::error!(error = %e, "Chief failed to send response (legacy)");
                        }
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "Chief stream error (legacy)");
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    }
                }
            }

            tracing::warn!("Chief listener stream ended, restarting in 10s");
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        }
    }

    /// Handle a single DM: send to LLM, execute any tool calls via SystemOperations, return text.
    async fn handle_dm(&self, user_msg: &str) -> Result<String> {
        tracing::debug!(content_len = user_msg.len(), "Chief handle_dm entered");

        let mut messages: Vec<Message> = vec![Message {
            role: Role::System,
            content: self.system_prompt.clone(),
        }];
        messages.push(Message {
            role: Role::User,
            content: user_msg.into(),
        });

        let mut rounds = 0;

        loop {
            tracing::debug!(round = rounds, "Chief sending to LLM");

            let request = ChatRequest {
                model: self.model.clone(),
                messages: messages.clone(),
                tools: Some(self.tools.clone()),
                max_tokens: Some(2048),
                temperature: None,
            };

            let response = self.provider.chat(request).await?;

            match &response.tool_calls {
                Some(calls) if !calls.is_empty() => {
                    if rounds >= self.max_tool_rounds {
                        tracing::debug!(rounds, "Chief hit max tool rounds");
                        return Ok(response.content.clone());
                    }
                    rounds += 1;

                    // Add assistant message with tool calls
                    messages.push(Message {
                        role: Role::Assistant,
                        content: response.content.clone(),
                    });

                    for call in calls {
                        tracing::info!(tool = %call.name, "Chief executing tool");
                        let result = self.execute_async_tool(call).await;
                        let result_val =
                            result.unwrap_or(serde_json::json!({"error": "tool execution failed"}));
                        tracing::info!(tool = %call.name, "Chief tool result: {}", serde_json::to_string(&result_val).unwrap_or_default().chars().take(200).collect::<String>());
                        let result_str = serde_json::to_string(&result_val)
                            .unwrap_or_else(|_| "{\"error\": \"serialization failed\"}".into());
                        messages.push(Message {
                            role: Role::Tool,
                            content: result_str,
                        });
                    }
                }
                _ => {
                    return Ok(response.content);
                }
            }
        }
    }

    async fn execute_async_tool(&self, call: &ToolCall) -> Option<serde_json::Value> {
        if let Some(executor) = self.executors.get(&call.name) {
            executor(call, self.system_ops.as_ref()).await
        } else {
            tracing::warn!(tool = %call.name, "Chief unknown tool called");
            Some(serde_json::json!({ "error": format!("Unknown tool: {}", call.name) }))
        }
    }
}
