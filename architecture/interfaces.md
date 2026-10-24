# Key Interfaces

All cross-crate async interfaces use `Pin<Box<dyn Future<Output = T> + Send + '_>>`. No `async-trait` crate.

## ChatSession (tj-chat)

```rust
pub trait ChatSession: Send + Sync {
    fn send(&self, message: &str) -> Pin<Box<dyn Future<Output = ChatResult<()>> + Send + '_>>;
    fn receive(&self) -> Pin<Box<dyn Future<Output = ChatResult<String>> + Send + '_>>;
}
```

- Implementations: `ZulipFlowSession`, `ZulipDmSession` (tj-chat-zulip)
- Internal: `tokio::sync::mpsc` channels, async HTTP via reqwest
- Manager creates sessions, passes `Arc<dyn ChatSession>` to Flow for lifecycle notifications
- StageExecutor holds `Option<Arc<dyn ChatSession>>` for multi-turn conversations

## FlowDatabase (tj-db)

Async trait defined in tj-db. `Database` implements by wrapping sync SQLite in `Box::pin(async move)`.

Methods: `save_flow`, `load_flow`, `save_stage`, `load_stage_by_name`, `complete_flow`, `update_flow_status`, `load_flows_for_run`, `load_stage`

## StageExecutor (tj-stage)

Concrete struct (not a trait). Holds `Option<Arc<dyn ChatSession>>` for multi-turn.

```rust
pub struct StageExecutor {
    db: Arc<Database>,
    tool: Arc<ToolExecutor>,
    config_loader: Arc<ConfigLoader>,
    chat_session: Option<Arc<dyn ChatSession>>,
}
```

Flow holds `Arc<StageExecutor>` (concrete), not `Arc<dyn StageExecutor>` (trait). This is a polymorphism gap — Flow is coupled to the concrete type.

Multi-turn loop in `run_multi_turn()`: stage output -> chat send -> user reply -> runner -> repeat until `::TURBOJET:DONE::` or `MAX_TURNS=100`. Caller sees one `execute()` call. WaitingForInput never escapes the executor.

## SystemOperations (tj-agent)

Async trait defined in tj-agent (dependency inversion). Implemented by `ManagerHandle` in tj-manager.

Methods: `get_job`, `list_jobs`, `create_job`, `cancel_job`, `list_flows`, `approve_gate`, `create_project`, `list_projects`, `list_lanes`

Concierge holds `Arc<dyn SystemOperations>` -- never knows Manager's concrete type.
