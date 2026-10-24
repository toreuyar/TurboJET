# TurboJET Rust Rewrite — System Architecture

> **Historical design record.** This early Rust rewrite sketch predates the
> current registry, execution-bundle, and workflow-package design. Its
> `smg_path`/`adc_dir`/`schema_dir`/`prompt_dir` fields and its example default
> graph path are not the current configuration contract. Current configuration uses
> `workflow_sources` (`file`, `directory`, `package`); package workflows carry
> their own agents, prompts, and schemas under `workflows/<name>/`. The legacy
> `adc_dir` is optional and consulted only by old file/directory graphs with bare
> agent ids. See [configuration](configuration.md) and
> [workflow authoring](workflow-authoring.md).

## Crates

```
turbojet/
  Cargo.toml                    # workspace
  crates/
    tj-core/                    # Manager, SMG, ADC, state machine, storage
    tj-api/                     # OpenAI-compatible HTTP server
    tj-chef/                    # Chef LLM agent
    tj-runner/                  # Runner trait + implementations
    tj-provider/                # LLM provider clients
    tj-config/                  # Config types, loading, validation
  config/                       # Runtime config files (SMG, ADC, schemas, prompts)
  docker/                       # Dockerfiles for runners
```

Why workspace + crates: each crate compiles independently, clean dependency boundaries, testable in isolation. No circular deps possible.

---

## tj-config

Config loading. Zero business logic.

### Types

```rust
// SMG types
struct StateMachineGraph {
    id: String,
    flows: HashMap<String, Flow>,
    stages: HashMap<String, Stage>,
    limits: Limits,
}

struct Flow {
    id: String,
    name: String,
    stages: Vec<String>,
    gate: Option<Gate>,
    routes: Vec<Route>,
}

struct Stage {
    id: String,
    name: String,
    flow: String,
    agent_config: String,  // ADC file path
    runner_type: RunnerType,  // Llm | Docker
    timeout_secs: u64,
}

struct Route {
    from: String,
    condition: RouteCondition,
    to: String,
}

enum RouteCondition {
    OutputField { field: String, value: String },
    GateApproved,
    GateRejected,
    Always,
}

struct Limits {
    iteration: u32,  // per stage, default 3
    steps: u32,      // per flow, default 20
    depth: u32,      // global, default 5
}

struct Gate {
    id: String,
    auto_approve: bool,
}
```

```rust
// ADC types
struct AgentDefinitionConfig {
    id: String,
    name: String,
    flow: String,
    prompt_path: String,
    tools: Vec<String>,
    disabled_tools: Vec<String>,
    timeout_secs: u64,
    permissions: Vec<PermissionRule>,
    input_artifacts: Vec<ArtifactDef>,
    output_artifacts: Vec<ArtifactDef>,
}

struct PermissionRule {
    tool: String,
    pattern: String,
    action: PermissionAction,
}

enum PermissionAction { Allow, Deny }

struct ArtifactDef {
    name: String,
    schema_path: String,
}
```

```rust
// System config
struct TurboJetConfig {
    listen_addr: String,           // "0.0.0.0:8100"
    database_path: String,         // "~/.turbojet/state.db"
    smg_path: String,              // deprecated single-file alias; use workflow_sources
    adc_dir: String,               // optional legacy root; package graphs never use it
    schema_dir: String,            // historical field; not part of the current config
    prompt_dir: String,            // historical field; prompts live in each package
    providers: Vec<ProviderConfig>,
    docker: DockerConfig,
    chef: ChefConfig,
}

struct ProviderConfig {
    id: String,
    provider_type: ProviderType,   // Anthropic | OpenAI | Gemini | ...
    api_key_env: String,
    base_url: String,
    default_model: String,
}

struct DockerConfig {
    socket_path: String,
    runner_images: HashMap<String, String>,  // runner_name -> image
    mount_base: String,
}

struct ChefConfig {
    provider_id: String,
    model: String,
    system_prompt: String,
}
```

### Functions

```rust
fn load_smg(path: &Path) -> Result<StateMachineGraph>;
fn load_adc(dir: &Path, stage_id: &str) -> Result<AgentDefinitionConfig>;
fn load_system_config(path: &Path) -> Result<TurboJetConfig>;
fn validate_schema(artifact: &Value, schema_path: &Path) -> Result<()>;
```

---

## tj-core

The Manager. The main loop. No HTTP, no LLM calls — pure state machine logic.

### Types

```rust
struct PipelineRun {
    id: RunId,
    status: RunStatus,
    current_flow: String,
    current_stage: String,
    completed_stages: Vec<String>,
    artifacts: HashMap<String, Value>,
    gate_state: Option<GateState>,
    iteration_counts: HashMap<String, u32>,
    step_counts: HashMap<String, u32>,
    total_depth: u32,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

enum RunStatus {
    Running,
    WaitingForGate,
    WaitingForUser,
    Completed,
    Failed,
    Cancelled,
}

struct GateState {
    gate_id: String,
    stage: String,
    summary: String,
    decision: Option<GateDecision>,
}

enum GateDecision {
    Approved,
    Rejected { reason: String },
}

struct StageResult {
    stage_id: String,
    status: StageStatus,
    output: Option<Value>,
    error: Option<String>,
}

enum StageStatus {
    Success,
    Failed { reason: String },
    Timeout,
}
```

### Manager

```rust
struct Manager {
    smg: StateMachineGraph,
    adc_cache: HashMap<String, AgentDefinitionConfig>,
    store: Box<dyn StateStore>,
}

impl Manager {
    fn new(smg: StateMachineGraph, store: Box<dyn StateStore>) -> Result<Self>;
    
    // Run lifecycle
    fn create_run(&self, description: String) -> Result<PipelineRun>;
    fn get_run(&self, id: &RunId) -> Result<Option<PipelineRun>>;
    fn list_runs(&self) -> Result<Vec<PipelineRun>>;
    fn cancel_run(&self, id: &RunId, reason: String) -> Result<()>;
    
    // Stage execution — returns what to do next
    fn next_action(&self, run: &PipelineRun) -> ManagerAction;
    fn apply_result(&self, run_id: &RunId, result: StageResult) -> Result<PipelineRun>;
    fn apply_gate_decision(&self, run_id: &RunId, decision: GateDecision) -> Result<PipelineRun>;
}

enum ManagerAction {
    ExecuteStage { stage_id: String, adc: AgentDefinitionConfig, input: Value },
    WaitGate { gate_id: String, summary: String },
    NotifyUser { message: String },
    Complete,
    Fail { reason: String },
}
```

This is the key design: `Manager` doesn't execute anything. It returns `ManagerAction` — the caller decides what to do with it. The Manager is pure logic.

### State Store

```rust
trait StateStore: Send + Sync {
    fn save_run(&self, run: &PipelineRun) -> Result<()>;
    fn load_run(&self, id: &RunId) -> Result<Option<PipelineRun>>;
    fn list_runs(&self) -> Result<Vec<PipelineRun>>;
    fn delete_run(&self, id: &RunId) -> Result<()>;
}

struct SqliteStore { /* ... */ }
impl StateStore for SqliteStore { /* ... */ }
```

---

## tj-runner

Runner trait and implementations.

```rust
trait Runner: Send + Sync {
    fn runner_type(&self) -> &str;
    fn execute(&self, task: TaskDefinition) -> Pin<Box<dyn Future<Output = Result<StageResult>> + Send>>;
}

struct TaskDefinition {
    stage_id: String,
    prompt: String,
    input_artifacts: Value,
    output_dir: PathBuf,
    timeout_secs: u64,
    adc: AgentDefinitionConfig,
}
```

### LlmRunner

For D1, D2, P3-P5. Direct LLM call, no container.

```rust
struct LlmRunner {
    provider: Box<dyn LlmProvider>,
}

impl Runner for LlmRunner {
    // Calls provider.chat(prompt + artifacts) 
    // Parses response into StageResult
}
```

### DockerRunner

For E6-E11, D12-D15. Spawns container, waits, reads output.

```rust
struct DockerRunner {
    docker: Docker,        // bollard client
    config: DockerConfig,
}

impl Runner for DockerRunner {
    // 1. Create container with mounted volumes
    // 2. Inject prompt + artifacts into mounted input dir
    // 3. Start container
    // 4. Wait for exit or watchdog timeout
    // 5. Read output artifacts from mounted output dir
    // 6. Remove container
    // 7. Return StageResult
}
```

---

## tj-provider

LLM provider clients. Each implements a simple chat trait.

```rust
trait LlmProvider: Send + Sync {
    fn chat(&self, request: ChatRequest) -> Pin<Box<dyn Future<Output = Result<ChatResponse>> + Send>>;
    fn chat_stream(&self, request: ChatRequest) -> Pin<Box<dyn Stream<Item = Result<StreamChunk>> + Send>>;
    fn list_models(&self) -> Vec<ModelInfo>;
}

struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    tools: Option<Vec<ToolDef>>,
    max_tokens: Option<u32>,
    temperature: Option<f32>,
}

struct ChatResponse {
    content: String,
    tool_calls: Option<Vec<ToolCall>>,
    usage: TokenUsage,
}

enum StreamChunk {
    TextDelta(String),
    ToolCallDelta(ToolCallDelta),
    Done(ChatResponse),
}
```

Implementations:
```rust
struct AnthropicProvider { /* api_key, base_url, client */ }
struct OpenAiProvider { /* api_key, base_url, client */ }
struct GeminiProvider { /* api_key, base_url, client */ }
```

---

## tj-chef

The user-facing LLM agent. Minimal — it's a messenger.

```rust
struct Chef {
    provider: Box<dyn LlmProvider>,
    model: String,
    system_prompt: String,
    session_store: Box<dyn ChefSessionStore>,
}

struct ChefSession {
    id: String,
    user_id: String,
    run_id: Option<RunId>,
    messages: Vec<Message>,
}

impl Chef {
    async fn handle_message(&mut self, session: &mut ChefSession, user_msg: String) -> Result<ChefResponse>;
    fn notify_pipeline_event(&self, session: &mut ChefSession, event: PipelineEvent) -> String;
}

enum ChefResponse {
    Text(String),
    NeedsApproval { gate_id: String, summary: String },
    PipelineStarted { run_id: RunId },
}

enum PipelineEvent {
    StageCompleted { stage: String, summary: String },
    GateReached { gate_id: String, flow: String, summary: String },
    PipelineCompleted { summary: String },
    PipelineFailed { reason: String },
}
```

Chef's system prompt instructs it to:
1. Relay pipeline events to the user in natural language
2. Ask for gate approvals
3. Forward user decisions back
4. NOT make pipeline decisions itself

---

## tj-api

OpenAI-compatible HTTP server.

### Endpoints

```
POST /v1/chat/completions    → chef.handle_message()  (SSE streaming)
GET  /v1/models              → list available models

GET  /v1/pipeline/runs       → manager.list_runs()
POST /v1/pipeline/runs       → manager.create_run()
GET  /v1/pipeline/runs/:id   → manager.get_run()
POST /v1/pipeline/runs/:id/approve → manager.apply_gate_decision()
POST /v1/pipeline/runs/:id/cancel  → manager.cancel_run()
```

The `/v1/chat/*` endpoints are the OpenAI-compatible layer — they go through Chef.
The `/v1/pipeline/*` endpoints are direct Manager access (for programmatic clients).

### Wiring

```rust
struct AppState {
    manager: Arc<Manager>,
    chef: Arc<Mutex<Chef>>,
    runners: HashMap<String, Arc<Box<dyn Runner>>>,
    config: Arc<TurboJetConfig>,
}

async fn main() {
    let config = load_system_config("config/turbojet.toml")?;
    let smg = load_smg(&config.smg_path)?;
    let store = SqliteStore::new(&config.database_path)?;
    let manager = Manager::new(smg, Box::new(store))?;
    
    let providers = build_providers(&config)?;
    let chef = Chef::new(/* ... */);
    let runners = build_runners(&config, &providers)?;
    
    let state = AppState { manager, chef, runners, config };
    
    let app = Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/models", get(list_models))
        .route("/v1/pipeline/runs", get(list_runs).post(create_run))
        .route("/v1/pipeline/runs/{id}", get(get_run))
        .route("/v1/pipeline/runs/{id}/approve", post(approve_gate))
        .route("/v1/pipeline/runs/{id}/cancel", post(cancel_run))
        .with_state(Arc::new(state));
    
    let listener = TcpListener::bind(&config.listen_addr).await?;
    axum::serve(listener, app).await?;
}
```

### The Main Loop

The event loop that ties everything together:

```rust
async fn run_pipeline_loop(state: Arc<AppState>, run_id: RunId) {
    loop {
        let run = state.manager.get_run(&run_id)?.unwrap();
        let action = state.manager.next_action(&run);
        
        match action {
            ManagerAction::ExecuteStage { stage_id, adc, input } => {
                let runner = pick_runner(&state, &adc);
                let task = TaskDefinition::new(&stage_id, &adc, input);
                let result = runner.execute(task).await;
                state.manager.apply_result(&run_id, result)?;
                
                // Notify chef
                let session = state.chef.get_session_for_run(&run_id);
                state.chef.notify_pipeline_event(session, PipelineEvent::StageCompleted { ... });
            }
            
            ManagerAction::WaitGate { gate_id, summary } => {
                // Notify chef to ask user
                let session = state.chef.get_session_for_run(&run_id);
                state.chef.notify_pipeline_event(session, PipelineEvent::GateReached { ... });
                break; // Stop loop, wait for gate decision via API
            }
            
            ManagerAction::NotifyUser { message } => { /* send to chef */ }
            ManagerAction::Complete => { /* notify, persist, break */ break; }
            ManagerAction::Fail { reason } => { /* notify, persist, break */ break; }
        }
    }
}
```

When a gate is reached, the loop stops. When the user approves via `POST /v1/pipeline/runs/:id/approve`, the API handler calls `manager.apply_gate_decision()` and spawns a new `run_pipeline_loop`.

---

## Dependency Graph

```
tj-api
  ├── tj-core
  │     └── tj-config
  ├── tj-chef
  │     ├── tj-provider
  │     └── tj-config
  ├── tj-runner
  │     ├── tj-provider
  │     └── tj-config
  └── tj-config
```

No circular deps. tj-config is the leaf. tj-core has no knowledge of HTTP or LLM.

---

## What Gets Reused from TypeScript

| Asset | Action |
|-------|--------|
| `default.smg.json` (18 stages, 4 flows) | Copy as-is, Rust types match the schema |
| 18 ADC files | Copy as-is, Rust types match the schema |
| 18 JSON schemas | Copy as-is, used for artifact validation |
| 18 agent prompts | Copy as-is, injected into runners |
| `permissions/default.json` | Copy as-is |
| TypeScript engine logic | Rewrite in Rust — ~800 lines, straightforward |
| Tools, OpenCode integration | Discard entirely |

---

## Build Order

1. **tj-config** — types + loaders + validation. Testable standalone.
2. **tj-core** — Manager + StateStore. Testable with mock store.
3. **tj-provider** — LLM client trait + one implementation (start with OpenAI-compatible).
4. **tj-runner** — LlmRunner first (simpler), DockerRunner second.
5. **tj-chef** — minimal agent loop.
6. **tj-api** — wire everything together, Axum server.

Each crate builds and tests independently. No need to build all at once.
