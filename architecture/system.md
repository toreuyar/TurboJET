# System Architecture

```
System Level (peers, each async with own event loop)
  +-- Manager (implements SystemOperations)
  |     +-- API request -> create Flow -> flow.execute().await
  |     |                          |
  |     |                     Flow (async methods, sync domain logic)
  |     |                       +-- FlowDatabase (async trait, tj-db)
  |     |                       +-- StageExecutor (async trait, tj-stage)
  |     |                             +-- ToolExecutor
  |     |                             |    +-- Runner (async HTTP)
  |     |                             +-- ChatSession (async, multi-turn)
  |     |
  |     +-- FlowOutcome -> route, dispatch next
  |
  +-- Concierge (holds Arc<dyn SystemOperations>)
  |     +-- DM ChatSession (async)
  |     +-- LLM Provider (async)
  |     +-- Calls Manager via SystemOperations trait
  |
  +-- API -> routes to Manager or Concierge
```

## Layer Responsibilities

| Layer | Crate | Owns | Does NOT own |
|-------|-------|------|-------------|
| Manager | tj-manager | Event loop, flow dispatch, system interfaces, gate approval trigger | Chat, state machines, LLM ops, watchdog timing |
| Concierge | tj-agent | DM chat, LLM interaction, system ops calls | Flow execution, state management |
| Flow | tj-flow | State machine, stage routing, outcome evaluation, watchdog timing | Runtime awareness, chat |
| Stage | tj-stage | ADC loading, prompt building, tool dispatch, multi-turn conversations | Lifecycle (created/run/torn down by Flow) |
| Tool | tj-tool | Concrete work execution | -- |
| Runner | tj-runner | Async HTTP to execution environments | -- |

## Key Rules

- **Manager is a thin dispatcher.** Dispatches Flows, handles outcomes. Zero chat. Zero spawn_blocking.
- **Concierge is a peer.** Own event loop. Calls Manager via `SystemOperations` trait. Not a child.
- **Flow is self-contained.** Created, executed, done. State on `self`. Async `execute`/`resume`. No wrapper.
- **Stage has no lifecycle.** Created, run, and torn down by Flow as a function call.
- **Async propagates to the event loop.** No spawn_blocking, no block_on, no inner runtimes.
- **`.await` only at external boundaries.** DB, HTTP, LLM. Domain logic between awaits is sync.
