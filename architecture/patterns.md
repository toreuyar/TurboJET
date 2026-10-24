# Known Patterns

- **DB as bridge:** Async Manager observes sync operations by reading DB
- **Worktree isolation:** Each flow gets a git worktree for clean repo state
- **Runner execution:** Stages run in Docker/OpenCode containers with mounted volumes
- **Event loop:** ManagerActor receives `ManagerEvent` via `mpsc::UnboundedReceiver`, dispatches to handler methods
- **Async trait pattern:** Most cross-crate async interfaces use `Pin<Box<dyn Future<Output = T> + Send + '_>>`. Exception: `ChatProvider` uses `async-trait` crate.
- **Single-use objects:** Flow is created, executed, returns outcome. No long-lived runners
- **FlowDatabase wrapping:** `Database` (sync rusqlite) implements async `FlowDatabase` trait by wrapping calls in `Box::pin(async move { ... })`. The sync call does not yield, so it runs on the calling tokio task. Acceptable for SQLite (<1ms ops) but not ideal.

## Post-Sprint State (Async Boundary Re-Engineering)

All major async boundary violations fixed:
- `ChatSession::send()` and `receive()` are now async (`Pin<Box<dyn Future>>`)
- Zulip sessions use `tokio::sync::mpsc` internally
- `FlowDatabase` is an async trait
- `SystemOperations` is an async trait (dependency inversion in tj-agent)
- `StageExecutor::execute()` is a concrete async method
- No `spawn_blocking`, no `block_on`, no `std::sync::mpsc` in the codebase
- Manager is a thin dispatcher with zero chat logic in the event loop

## State Machine Graph (SMG)

Configuration defines flows as nodes in a directed graph. Each flow has stages (except gate flows). Routes connect stage outcomes to next stages or cross-flow references. Loaded by `ConfigLoader`, consumed by Flow for stage routing.

## Migration Notes

- `container_*` fields renamed to `runner_*` across all structs and DB schema
- DB migration: checks `runner_host` first, falls back to `container_host` rename if needed
- Data flow: OpenCodeRunner -> StageResult -> ToolResult -> StageRecord -> DB -> Manager
