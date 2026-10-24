# TurboJET Architecture

Primary entry point for architecture documentation. Read this first, then follow links to relevant sections.

## Overview

TurboJET v2: autonomous SDLC orchestration service in Rust. AI-powered development pipelines with OpenAI-compatible HTTP API and pluggable LLM/chat providers.

## Documents

| Document | Summary |
|----------|---------|
| [ARCHITECTURAL_PRINCIPLES.md](ARCHITECTURAL_PRINCIPLES.md) | Design principles: OOP > SOLID > YAGNI, async boundary rules, crate boundaries, chat architecture, error handling |
| [system.md](system.md) | System architecture diagram, layer responsibilities (Manager/Concierge/Flow/Stage/Tool/Runner), key rules |
| [crates.md](crates.md) | Crate dependency graph, circular dependency constraints, per-crate ownership |
| [interfaces.md](interfaces.md) | Async trait definitions: ChatSession, FlowDatabase, StageExecutor, SystemOperations |
| [data-structures.md](data-structures.md) | Flow struct, FlowOutcome (3 variants), StageRecord, StageResult, ToolResult, Concierge struct |
| [gates.md](gates.md) | Gate flow type in SMG, no stages on gates, cross-flow route references, external trigger lifecycle |
| [patterns.md](patterns.md) | Known patterns (DB as bridge, worktree isolation, event loop), SMG description, migration notes |

## Quick Reference

- **Manager** = thin dispatcher. Zero chat. Zero spawn_blocking. Creates ChatSession, passes to Flow.
- **Concierge** = peer of Manager. Own event loop. Calls via `Arc<dyn SystemOperations>`.
- **Flow** = self-contained. Created, executed, returns `FlowOutcome`. State on `self`.
- **FlowOutcome** (tj-flow) = `Completed` | `Failed` | `TimedOut` | `WaitingForInput` (dead path). Manager's FlowOutcome = `Completed` | `Failed` | `TimedOut`.
- **Gates** = separate `type: "gate"` flows in SMG. Manager halts. No stages.
- **Async** = `.await` only at external boundaries. No block_on, no inner runtimes.
- **StageExecutor** = concrete struct (not a trait). Polymorphism gap.
- **ChatProvider** uses `async-trait` crate; other traits use manual `Pin<Box<dyn Future>>`.
