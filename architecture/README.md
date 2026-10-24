# Architecture

TurboJET v2: autonomous SDLC orchestration service in Rust. AI-powered development pipelines with OpenAI-compatible HTTP API and pluggable LLM/chat providers.

## Files

| File | Content |
|------|---------|
| [system.md](system.md) | System architecture diagram, layer responsibilities, key rules |
| [crates.md](crates.md) | Crate dependency graph, per-crate responsibilities |
| [interfaces.md](interfaces.md) | Key async trait interfaces: ChatSession, FlowDatabase, StageExecutor, SystemOperations |
| [data-structures.md](data-structures.md) | Flow, FlowOutcome, StageRecord, StageResult, ToolResult, Concierge |
| [gates.md](gates.md) | Gate flow type, external trigger, route schema |
| [patterns.md](patterns.md) | Known patterns, SMG, migration notes |
