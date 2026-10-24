# Architectural Principles

The architect agent reads this before every operation. Update freely -- no need to touch agent definitions.

## Priority: OOP > SOLID > YAGNI

When principles conflict, earlier wins.

### OOP

- **Encapsulation:** Objects own their data and behavior. Internals are private. Shared mutable state gets its own class/repository -- never crammed into a domain object.
- **Inheritance:** Trait inheritance for behavioral contracts. Composition over inheritance for struct hierarchies. Default trait implementations for shared behavior.
- **Polymorphism:** Dependencies behind `dyn Trait`. Consumers never know concrete types. `dyn Trait` at boundaries, generics within a crate.

### SOLID

- **S:** One responsibility per struct. Flow executes. Manager dispatches. ChatSession chats.
- **O:** New providers added by implementing traits, not modifying existing code.
- **L:** Trait implementations fully interchangeable.
- **I:** Narrow traits. `FlowDatabase` has only what Flow needs. `StageExecutor` has only `execute`. No god-traits.
- **D:** Both sides depend on abstractions. Flow depends on `StageExecutor` trait, not OpenCode executor.

### YAGNI

- No speculative interfaces. No future-proofing. No wrapper classes.
- Every struct/trait/method must justify its existence right now.
- If the object IS the thing, don't wrap it.

## Async Boundary Rules

- **No sync doors on async I/O.** If the implementation does async work, the interface MUST be async. Period.
- **No `spawn_blocking`.** No `block_on`. No inner runtimes. Async propagates to the event loop.
- **No `std::sync::mpsc` behind async polling.** Use `tokio::sync::mpsc`.
- **Async trait pattern:** `Pin<Box<dyn Future<Output = T> + Send + '_>>`. No `async-trait` crate.

## Crate Boundaries

- Each crate owns its domain. No leaking implementation details across boundaries.
- Orchestration (Manager) contains no infrastructure logic (chat, git, storage). Use traits.
- No circular dependencies.

## Chat Architecture

- All chat through `ChatSession` interface. No direct provider usage outside the chat crate.
- Sessions created once per flow lifecycle, passed to stages by reference.
- Chat routing (DM vs topic) is the session's responsibility, not the consumer's.
- Programmatic status messages sent at stage boundaries only -- never during multi-turn conversations.
- LLM-generated content extracted and formatted before posting. Never raw API envelopes.

## Error Handling

- Fail loudly. Silent failures are bugs.
- `anyhow::Result` for application code, typed errors for library interfaces.
- User-facing errors produce human-readable messages in chat.
