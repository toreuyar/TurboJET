# TurboJET Integration Spec: Chef/Manager/Stage Agent System

## Overview

This spec covers the integration of the three-agent architecture (Chef → Manager → Stage) into TurboJET, making the pipeline capable of real software development.

## Architecture

```
User ←→ Chef (OpenCode primary agent)
              ↕ (TurboJET plugin + custom tools — TypeScript code)
         Manager (pipeline state machine + SDK sessions)
              ↕ (OpenCode SDK sessions via promptAsync)
         Stage Agents (LLM with tools)
```

## Components to Build

### 1. OpenCode Plugin (`turbojet/.opencode/plugins/turbojet.ts`)

An OpenCode plugin that:
- Subscribes to `session.idle` events for background stage sessions
- Manages pipeline state (which stages are running, completed, failed)
- Triggers `promptAsync` on the Chef session when Manager needs attention
- Provides the bridge between the deterministic pipeline code and the Chef agent

```typescript
export const TurboJetPlugin: Plugin = async ({ client, project }) => {
  const pipelineState = new PipelineState()
  
  return {
    event: async ({ event }) => {
      if (event.type === 'session.idle') {
        const sessionId = event.properties.sessionID
        const stageResult = await pipelineState.handleStageCompletion(sessionId)
        if (stageResult.needsChefAttention) {
          await client.session.promptAsync({
            path: { id: pipelineState.chefSessionId },
            body: { parts: [{ type: 'text', text: stageResult.chefMessage }] },
          })
        } else if (stageResult.nextStage) {
          await pipelineState.startStage(stageResult.nextStage)
        }
      }
    },
  }
}
```

### 2. Custom Tools (`turbojet/.opencode/tools/`)

Chef calls these tools to interact with the pipeline:

**`turbojet_start`** — Start a new pipeline run
- Input: `{ description: string, stages?: string[] }`
- Creates a pipeline run in the DB
- Starts D1 intake stage
- Returns: run ID

**`turbojet_status`** — Check pipeline/run status
- Input: `{ runId?: string }`
- Returns: current stage, progress, pending questions, artifacts

**`turbojet_answer`** — Answer questions from a stage
- Input: `{ runId: string, answers: Record<string, string> }`
- Feeds answers back to the waiting stage
- Returns: updated status

**`turbojet_approve`** — Approve a gate (D2 Go/No-Go, E→D boundary)
- Input: `{ runId: string, gateId: string, approved: boolean, comment?: string }`
- Returns: next stage status

**`turbojet_list`** — List all pipeline runs
- Input: `{ status?: string }`
- Returns: list of runs with status

### 3. Manager State Machine (`turbojet/src/engine/manager.ts`)

The Manager is deterministic TypeScript code (not an LLM). It:

1. Reads structured JSON output from Stage Agents
2. Decides next action based on rules:
   - `status: "success"` → proceed to next stage
   - `status: "failure"` with `recoverable: true` → retry (up to N times)
   - `status: "failure"` with `recoverable: false` → escalate to Chef
   - `status: "needs-input"` → escalate to Chef via promptAsync
   - Gate stages (D2, P5, E→D) → pause and escalate to Chef for approval
3. Manages stage execution order based on pipeline config
4. Handles the sprint loop (E6→E7→E8→E9→E10→E11 per task, retry logic)

### 4. Stage Agent Executor (`turbojet/src/engine/stage-executor.ts`)

Runs a single stage as an OpenCode SDK session:

1. Creates an OpenCode session with the stage's agent prompt as system prompt
2. Sends the stage's context as the user message (artifacts from previous stages)
3. For coding stages (E8): enables tools (file read, write, bash)
4. For planning stages (P1-P5): returns structured JSON
5. Waits for completion (via event listener in plugin) or runs async via promptAsync
6. Returns structured result to Manager

### 5. promptAsync Wake-Up (`turbojet/src/engine/wake-up.ts`)

A module that triggers the Chef agent:

```typescript
export async function wakeChef(chefSessionId: string, message: string): Promise<void> {
  const client = createOpencodeClient({
    baseUrl: 'http://127.0.0.1:4096',
    headers: {
      Authorization: 'Basic ' + Buffer.from(`${username}:${password}`).toString('base64'),
    },
  })
  
  await client.session.promptAsync({
    path: { id: chefSessionId },
    body: { parts: [{ type: 'text', text: message }] },
  })
}
```

## Stage Execution Modes

### Synchronous (blocking)
- D1, D2 — interactive, user-facing
- P1-P5 — fast planning stages (~30 seconds each)
- D12-D15 — fast delivery stages
- Use OpenCode Task subagent — blocks Chef until complete
- Result returns directly to Chef

### Asynchronous (non-blocking)
- E8 (Implement) — can run 10+ minutes
- E9 (Test) — can run several minutes
- Use SDK `promptAsync` on a separate session
- Plugin listens for `session.idle` event
- Manager processes result, decides next step
- Only escalates to Chef if needed

### Sprint Loop (E6→E11)
- Manager runs E6-E7 synchronously (fast analysis/design)
- E8 runs asynchronously (long coding task)
- Plugin detects E8 completion → Manager kicks off E9 (async)
- E10, E11: sync if fast, async if long
- Retry loop: E9/E10/E11 failure → back to E7 (up to 2 retries)
- All retries exhausted → escalate to Chef

## Integration with Existing Code

### What Changes
- `PipelineRunner` → becomes part of the Manager state machine
- `SprintController` → integrated into Manager's sprint loop logic
- `cli.ts` → replaced by Chef agent (user talks to Chef, not CLI)
- Stage implementations (D1-E11, D12-D15) → called via Stage Agent Executor instead of directly
- `AIClient` → extended with `promptAsync` support for wake-up

### What Stays
- All stage implementations (D1-D2, P1-P5, E6-E11, D12-D15)
- Agent prompts (`src/engine/agents/`)
- JSON schemas (`src/engine/schemas/`)
- SQLite DB and state management
- Event bus
- Stage type definitions

## Implementation Order

### Phase 1: Foundation
1. Create OpenCode plugin skeleton (`turbojet/.opencode/plugins/turbojet.ts`)
2. Create custom tools (`turbojet/.opencode/tools/turbojet_*.ts`)
3. Create wake-up module (`src/engine/wake-up.ts`)
4. Create Manager state machine (`src/engine/manager.ts`)

### Phase 2: Stage Execution
5. Create Stage Agent Executor (`src/engine/stage-executor.ts`)
6. Wire Manager to Stage Agent Executor
7. Wire plugin event listener to Manager
8. Test D1→D2 end-to-end through Chef

### Phase 3: Full Pipeline
9. Wire P1-P5 stages (autonomous, sync)
10. Wire E6-E11 stages with sprint loop (E8 async)
11. Wire D12-D15 stages
12. Implement E→D gate (Chef approval)

### Phase 4: Real Execution
13. E8 with real coding tools (file read/write, bash)
14. E9 with real test execution
15. End-to-end pipeline test: D1 → D15 with a real feature request

## Chef Agent Configuration

The Chef agent will be configured in `turbojet/opencode.json`:

```json
{
  "agent": {
    "chef": {
      "mode": "primary",
      "description": "TurboJet project manager — orchestrates the full SDLC pipeline",
      "prompt": "{file:./.opencode/prompts/chef.md}",
      "permission": {
        "edit": "allow",
        "bash": "allow",
        "task": { "turbojet-*": "allow", "*": "deny" }
      }
    },
    "manager": {
      "mode": "subagent",
      "hidden": true,
      "description": "Pipeline manager — executes stages and manages progress",
      "prompt": "{file:./.opencode/prompts/manager.md}"
    }
  }
}
```

## Testing Strategy

- Unit tests for Manager state machine (transitions, retry logic, gate handling)
- Unit tests for wake-up module
- Integration tests for plugin event handling
- End-to-end test: Chef receives user request → starts pipeline → D1→D2→...→D15
- Test with real OpenCode server for wake-up validation
