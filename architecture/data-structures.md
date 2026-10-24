# Key Data Structures

## Flow (tj-flow)

```rust
pub struct Flow {
    pub db: Arc<dyn FlowDatabase>,
    pub stage_executor: Arc<dyn StageExecutor>,
    pub config_loader: Arc<ConfigLoader>,
    pub config: FlowConfig,
    // State on self
    flow_id: String,
    run_id: String,
    flow_name: String,
    artifacts: HashMap<String, serde_json::Value>,
    current_stage: String,
    iteration_counts: HashMap<String, u32>,
    steps: u32,
    worktree: Option<WorktreeInfo>,
}
```

Methods: `async fn execute(mut self) -> FlowOutcome`, `async fn resume(mut self, completed_stage: &str) -> FlowOutcome`

## FlowOutcome (tj-flow)

Four variants: `Completed`, `Failed`, `TimedOut`, `WaitingForInput`.

Architecture intent is three variants (no WaitingForInput). The WaitingForInput variant is a dead path — StageExecutor's multi-turn loop handles it internally. Flow still produces it defensively for stages without ChatSession. Manager's `outcome_to_event()` converts it to Completed with a warning.

Manager's `events::FlowOutcome` (separate type) has only three variants: Completed, Failed, TimedOut. No WaitingForInput there.

No `HitGate` variant. Gate handling is Manager-level via `SystemOperations::approve_gate`.

## StageRecord (tj-db)

`id`, `flow_id`, `stage_name`, `status: StageStatus`, `input_artifacts`, `output_artifacts`, `error`, `iteration: u32`, `created_at`, `started_at`, `completed_at`, `runner_host`, `runner_port`, `runner_password`, `runner_session_id`

## StageResult (tj-runner) / ToolResult (tj-tool)

Same runner fields as StageRecord: `stage_id`, `status`, `output`, `error`, `runner_host`, `runner_port`, `runner_password`, `runner_session_id`

## Concierge (tj-agent)

```rust
pub struct Concierge {
    system_ops: Arc<dyn SystemOperations>,
    chat_session: Arc<dyn ChatSession>,
    llm_provider: Arc<dyn LlmProvider>,
}
```

Own async event loop: receive DM -> LLM -> system ops or respond.
