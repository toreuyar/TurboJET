# Architectural Rules

This file defines design principles and rules for TurboJET's architecture.
The architect agent reads this before every operation. Update freely — no need to touch agent definitions.

## Interfaces & Abstractions

- Prefer trait-based interfaces for all cross-crate boundaries (e.g., `ChatSession`, `ChatProvider`).
- Consumers should never know about provider internals (Zulip, Slack, etc.).
- Interfaces should be simple and dumb — minimal surface area, encapsulate complexity inside the implementation.
- Create interfaces at the point of consumption, not speculation. Build them when a second consumer appears or a clear boundary is needed.

## Crate Boundaries

- Each crate owns its domain. No cross-boundary leaking of implementation details.
- Orchestration (manager) must not contain infrastructure logic (chat, git, storage). Use interfaces.
- The database is the sync/async bridge. Only the manager is async. Everything else is sync.
- No circular dependencies between crates.

## Sync vs Async

- Only the manager runs async. Everything else (flow, stage, tool) is synchronous and blocking.
- Async operations that must be called from sync context use `tokio::spawn` (fire-and-forget) or channels (request-response). Never `block_in_place` + `block_on`.
- The manager does async I/O (conversation creation, message sending) BEFORE spawning blocking tasks. Results are passed as data.

## Chat Architecture

- All chat operations go through `ChatSession` interface. No direct chat provider usage outside the chat crate.
- Sessions are created once per flow lifecycle, passed by reference to stages.
- Concierge uses DM sessions. Stage agents use flow sessions. They call the same interface.
- Chat routing (DM vs topic) is the session's responsibility, not the consumer's.

## Messaging

- Programmatic status messages (flow/stage started/completed) are sent by the system, not LLM.
- These messages go through the ChatSession at stage boundaries only — never during multi-turn agent conversations.
- LLM-generated content is extracted and formatted before posting to chat. Never post raw API envelopes.

## Error Handling

- Fail loudly and clearly. Silent failures are bugs.
- Use `anyhow::Result` for application code, typed errors for library interfaces.
- Errors that affect users must produce human-readable messages in chat.
