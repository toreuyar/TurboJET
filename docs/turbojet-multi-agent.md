# TurboJET Multi-Agent Architecture

> **Historical design record (2026-05).** This describes an early proposed
> multi-agent/API-key design, not the current runtime. The current model is a
> single Chief conversational agent plus declarative workflows. Agent
> definitions, prompts, and schemas now live inside self-contained workflow
> packages under `workflows/<name>/` (or a private package under
> `config/local/`); there is no shipped shared agent or schema tree. For current
> behavior see [ARCHITECTURE.md](../ARCHITECTURE.md) and
> [workflow authoring](workflow-authoring.md).

## Overview

Single `/v1/chat/completions` endpoint. API key in `Authorization: Bearer <key>` header determines which agent handles the request. No user-side agent selection.

## Agent Roles

### Concierge (renamed from Chef)
- First point of contact for external users (developers, PMs)
- Start/monitor Jobs, approve Gates, list Flows
- Tools: start_job, list_jobs, get_job, approve_gate, cancel_job, merge_completed, list_flows
- Cannot modify system config, cannot create users

### Admin
- System administration, internal use only
- Create/manage API keys for Concierge users
- Manage projects, assign project-level permissions
- Configure external integrations (GitHub, GitLab, ADO tokens)
- Set project-level settings

### Architect
- Design new Flows, Stages, and agents dynamically
- Modify the SMG at runtime
- Register new runner agents in the system
- System-level design authority

## Routing Flow

1. Request hits `/v1/chat/completions`
2. Auth middleware extracts API key from header
3. Look up key in DB → resolves to (agent_id, user_id)
4. Load agent config from DB (system prompt path, tool set, model override)
5. Route to the appropriate agent handler

## Config Storage

**DB stores (live configurable):**
- Agent registry (agent_id, name, type, system_prompt_path, tool_set_id, model_override, enabled)
- API keys (key_hash, agent_id, user_id, permissions, created_at, expires_at)
- Users (user_id, name, email, role)
- Projects (deferred for now)

**Filesystem stores (historical proposal; superseded):**
- Workflow graphs: one root `workflow.smg.json` per package under `workflows/<name>/` (or a private package under `config/local/`)
- Agent definitions and prompts: `agents/<id>/agent.json` and `agents/<id>/prompt.md` inside that package
- Output schemas: `schemas/*.json` inside that package
- No shared agent/schema directory is shipped; `adc_dir` remains only for legacy `file`/`directory` graphs with bare agent ids

## Bootstrap

- First-start DB migration seeds pre-defined agents (Concierge, Admin, Architect)
- Default API key for Admin created on first run
- Admin then creates keys for Concierge users via Admin agent

## Zettelkasten References

- `20260514T105346117588000` — Multi-Agent API Key Routing
- `20260514T105359457441000` — Agent Roles: Concierge, Admin, Architect
- `20260514T105414512347000` — Config Storage: DB Registry + Filesystem
