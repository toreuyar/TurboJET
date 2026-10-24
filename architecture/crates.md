# Crate Dependency Graph

```
tj-api
  +-- tj-manager
  |     +-- tj-flow
  |     |     +-- tj-stage
  |     |     |     +-- tj-tool
  |     |     |     |     +-- tj-runner
  |     |     |     |           +-- tj-provider
  |     |     |     |           +-- tj-config
  |     |     |     +-- tj-db
  |     |     |     +-- tj-chat        (multi-turn conversations)
  |     |     +-- tj-db                (FlowDatabase trait)
  |     +-- tj-agent (Concierge)
  |     |     +-- tj-provider
  |     |     +-- tj-config
  |     |     +-- tj-chat              (DM chat sessions)
  |     +-- tj-config
  |     +-- tj-db
  +-- tj-provider
  +-- tj-runner
        +-- tj-config
        +-- tj-provider
```

No circular dependencies. `tj-flow` does NOT depend on `tj-chat` -- chat is Stage's responsibility.

## Current Issues

- `tj-stage`: `StageExecutor` is a concrete struct, not a trait. `Flow` holds `Arc<StageExecutor>` directly, violating polymorphism principle.
- `tj-chat`: Uses `async-trait` crate for `ChatProvider` trait, while all other async traits use manual `Pin<Box<dyn Future>>` pattern.
- `tj-agent`: Contains legacy `AgentContext` sync trait alongside new `SystemOperations` async trait. `Agent` struct uses sync AgentContext; `Concierge` uses async SystemOperations.
