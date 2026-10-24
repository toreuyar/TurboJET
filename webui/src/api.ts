import type { AuthResponse, ChatMsg, DeliveryBundle, DeliveryDecision, DeliveryEnvironment, DeliveryKind, DeliveryRelease, GateDecision, GateDecisionReceipt, Lane, PacketBundle, PacketRevision, PendingEffect, PlanBundle, PlanRevision, Project, QuestionAnswer, QuestionAnswerInput, Readiness, RecoveryState, RetryResult, RunBudget, RunDiagnostics, RunPublication, RunTree, StageLog, StageQuestion, TaskReadiness, TjArtifact, TjEvent, TjRun, User, WorkflowProfile, WorkflowVersion, WorkflowConfigured, RunOutput } from './types';

export type ApiError = Error & { status?: number };

let authToken: string | null = localStorage.getItem('tj-token');

export function getToken() { return authToken; }
export function setToken(t: string | null) {
  authToken = t;
  if (t) localStorage.setItem('tj-token', t);
  else localStorage.removeItem('tj-token');
}

/** Stable per-submission key; duplicate submits replay instead of appending. */
export function newIdempotencyKey(): string {
  const c = globalThis.crypto as Crypto | undefined;
  if (c?.randomUUID) return c.randomUUID();
  return `tj-${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

async function j<T>(path: string, opts?: RequestInit): Promise<T> {
  const headers: Record<string, string> = { ...(opts?.headers as Record<string, string> | undefined) };
  if (authToken) headers['Authorization'] = `Bearer ${authToken}`;
  const r = await fetch(path, { ...opts, headers, credentials: 'include' });
  if (!r.ok) {
    if (r.status === 401 && authToken) {
      setToken(null);
      window.dispatchEvent(new Event('tj-auth-expired'));
    }
    const body = await r.json().catch(() => ({}));
    const err = new Error(body.error || `HTTP ${r.status}`) as ApiError;
    err.status = r.status;
    throw err;
  }
  return r.json() as Promise<T>;
}

export const api = {
  login: (username: string, password: string) =>
    j<AuthResponse>('/v1/auth/login', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ username, password }) }),
  register: (username: string, password: string) =>
    j<AuthResponse>('/v1/auth/register', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ username, password }) }),
  logout: () => j<{ ok: boolean }>('/v1/auth/logout', { method: 'POST' }),
  me: () => j<User>('/v1/me'),
  readiness: () => j<Readiness>('/v1/readiness'),
  users: () => j<User[]>('/v1/users'),
  createUser: (username: string, password: string, display_name?: string, is_admin?: boolean) =>
    j<User>('/v1/users', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ username, password, display_name, is_admin }) }),
  projects: () => j<Project[]>('/v1/projects'),
  runs: () => j<TjRun[]>('/v1/pipeline/runs'),
  tree: (id: string) => j<RunTree>(`/v1/pipeline/runs/${encodeURIComponent(id)}/tree`),
  decisions: (id: string) => j<GateDecision[]>(`/v1/pipeline/runs/${encodeURIComponent(id)}/decisions`),
  events: (id: string) => j<TjEvent[]>(`/v1/pipeline/runs/${encodeURIComponent(id)}/events`),
  artifacts: (id: string) => j<TjArtifact[]>(`/v1/pipeline/runs/${encodeURIComponent(id)}/artifacts`),
  forkArtifact: (aid: string) => j<TjArtifact>(`/v1/pipeline/artifacts/${encodeURIComponent(aid)}/fork`, { method: 'POST' }),
  approve: (id: string, request: { approved: boolean; reason?: string; gate_instance_id: string; expected_state_version: number; idempotency_key: string; packet_revision_id?: string; packet_content_hash?: string; plan_revision_id?: string; plan_content_hash?: string; output_artifact_id?: string; output_content_hash?: string; output_stage_name?: string; acknowledge_incomplete?: boolean }) =>
    j<GateDecisionReceipt>(`/v1/pipeline/runs/${encodeURIComponent(id)}/approve`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ ...request, reason: request.reason ?? null }) }),
  cancel: (id: string) => j<TjRun>(`/v1/pipeline/runs/${encodeURIComponent(id)}/cancel`, { method: 'POST' }),
  budget: (id: string) => j<RunBudget>(`/v1/pipeline/runs/${encodeURIComponent(id)}/budget`),
  diagnostics: (id: string) => j<RunDiagnostics>(`/v1/pipeline/runs/${encodeURIComponent(id)}/diagnostics`),
  publication: (id: string) => j<RunPublication>(`/v1/pipeline/runs/${encodeURIComponent(id)}/publication`),
  activity: (id: string, after = 0) =>
    j<{ run_id: string; activity: StageLog[]; provider_attempts: unknown[]; next_after: number; has_more: boolean; resync: boolean }>(
      `/v1/pipeline/runs/${encodeURIComponent(id)}/activity?after=${after}`),
  downloadSupportExport: async (id: string) => {
    const headers: Record<string, string> = {};
    if (authToken) headers['Authorization'] = `Bearer ${authToken}`;
    const r = await fetch(`/v1/pipeline/runs/${encodeURIComponent(id)}/support-export`, { headers, credentials: 'include' });
    if (!r.ok) throw new Error((await r.json().catch(() => ({ error: r.statusText }))).error || `HTTP ${r.status}`);
    const blob = await r.blob();
    const url = URL.createObjectURL(blob);
    try {
      const anchor = document.createElement('a');
      anchor.href = url;
      anchor.download = `turbojet-support-${id}.json`;
      document.body.appendChild(anchor);
      anchor.click();
      anchor.remove();
    } finally {
      URL.revokeObjectURL(url);
    }
    return { downloaded: true };
  },
  recovery: (id: string) => j<{ recovery: RecoveryState | null; unresolved_effects: PendingEffect[] }>(`/v1/pipeline/runs/${encodeURIComponent(id)}/recovery`),
  retry: (id: string, acknowledgeUnknownEffects = false) =>
    j<RetryResult>(`/v1/pipeline/runs/${encodeURIComponent(id)}/retry`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ acknowledge_unknown_effects: acknowledgeUnknownEffects }),
    }),
  deleteRun: async (id: string) => {
    const headers: Record<string, string> = {};
    const token = getToken();
    if (token) headers['Authorization'] = `Bearer ${token}`;
    const r = await fetch(`/v1/pipeline/runs/${encodeURIComponent(id)}`, { method: 'DELETE', headers, credentials: 'include' });
    if (!r.ok) throw new Error((await r.json().catch(() => ({ error: r.statusText }))).error);
    return r.json() as Promise<{ deleted: boolean }>;
  },
  createRun: (description: string, project_id: string, lane_id: string) =>
    j<TjRun>('/v1/pipeline/runs', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ description, project_id, lane_id }) }),
  createProject: (name: string, options?: { repo_url?: string; workflow_id?: string; catalog_id?: string; profile?: string }) =>
    j<{ project: Project; lane: Lane | null; workflow_id?: string }>('/v1/projects', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ name, repo_url: options?.repo_url || null, workflow_id: options?.workflow_id, catalog_id: options?.catalog_id, profile: options?.profile }) }),
  profiles: () => j<{ profiles: WorkflowProfile[] }>('/v1/profiles'),
  output: (id: string) => j<RunOutput>(`/v1/pipeline/runs/${encodeURIComponent(id)}/output`),
  workflows: () => j<{ workflows: WorkflowVersion[]; configured: WorkflowConfigured | null }>('/v1/workflows'),
  workflow: (id: string) => j<WorkflowVersion>(`/v1/workflows/${encodeURIComponent(id)}`),
  importWorkflows: () => j<{ workflows: WorkflowVersion[] }>(`/v1/workflows/import`, { method: 'POST' }),
  lanes: () => j<{ lanes: Lane[] }>('/v1/lanes'),
  setLaneWorkflow: (laneId: string, workflowId: string | null) =>
    j<{ lane: Lane }>(`/v1/lanes/${encodeURIComponent(laneId)}/workflow`, { method: 'PUT', body: JSON.stringify({ workflow_id: workflowId }) }),
  stageLogs: (id: string, after = 0) =>
    j<StageLog[]>(`/v1/pipeline/runs/${encodeURIComponent(id)}/stage-logs?after=${after}`),
  sendInput: (id: string, text: string, opts?: { stageId?: string; attempt?: number; questionId?: string; idempotencyKey?: string }) =>
    j<{ status: string; seq?: number | null; question_id?: string | null; replayed?: boolean }>(`/v1/pipeline/runs/${encodeURIComponent(id)}/input`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        text,
        stage_id: opts?.stageId,
        attempt: opts?.attempt,
        question_id: opts?.questionId,
        idempotency_key: opts?.idempotencyKey,
      }),
    }),
  /** OQ-2: a structured question answer travels as the serialized QuestionAnswer
   *  JSON object inside POST /input's `text` field, bound to the durable
   *  question/stage/attempt identity (TJ-014). */
  sendQuestionAnswer: (id: string, answer: QuestionAnswer, opts: QuestionAnswerInput) =>
    api.sendInput(id, JSON.stringify({ selected: answer.selected, text: answer.text ?? undefined }), {
      stageId: opts.stageId,
      attempt: opts.attempt,
      questionId: opts.questionId,
      idempotencyKey: opts.idempotencyKey ?? newIdempotencyKey(),
    }),
  questions: (id: string, openOnly = true) =>
    j<{ questions: StageQuestion[] }>(`/v1/pipeline/runs/${encodeURIComponent(id)}/questions?status=${openOnly ? 'open' : 'all'}`),
  bundle: (id: string) =>
    j<{ id?: string; run_id: string; bundle_hash?: string; legacy: boolean; reproducible: boolean; bundle?: unknown; error?: string }>(`/v1/pipeline/runs/${encodeURIComponent(id)}/bundle`),
  inputStatus: (id: string) =>
    j<{ bridge_active: boolean; waiting: boolean; stage_messages: string[]; queued: { seq: number; text: string }[] }>(`/v1/pipeline/runs/${encodeURIComponent(id)}/input`),
  cancelInput: async (id: string, seq: number) => {
    const headers: Record<string, string> = { 'Content-Type': 'application/json' };
    const token = getToken();
    if (token) headers['Authorization'] = `Bearer ${token}`;
    const r = await fetch(`/v1/pipeline/runs/${encodeURIComponent(id)}/input`, { method: 'DELETE', headers, credentials: 'include', body: JSON.stringify({ seq }) });
    if (!r.ok) throw new Error((await r.json().catch(() => ({ error: r.statusText }))).error);
    return r.json() as Promise<{ cancelled: boolean }>;
  },
  runFileRaw: async (id: string, path: string) => {
    const headers: Record<string, string> = {};
    const token = getToken();
    if (token) headers['Authorization'] = `Bearer ${token}`;
    const r = await fetch(`/v1/pipeline/runs/${encodeURIComponent(id)}/files/${path.split('/').map(encodeURIComponent).join('/')}?raw=1`, { headers, credentials: 'include' });
    if (!r.ok) throw new Error((await r.json().catch(() => ({ error: r.statusText }))).error);
    return r.json() as Promise<{ path: string; mime: string; base64: string }>;
  },
  /** Live stage-log stream for one run. Falls back to polling on error. */
  stageLogStream: (runId: string, onLog: (rows: unknown[]) => void, onAuthError?: () => void, onLive?: () => void): (() => void) => {
    const token = authToken ?? '';
    let poll: ReturnType<typeof setInterval> | null = null;
    let after = 0;
    let closed = false;
    let busy = false;
    const catchup = async () => {
      if (busy || closed) return;
      busy = true;
      try {
        const rows = await api.stageLogs(runId, after);
        if (closed) return;
        if (rows.length) { after = Math.max(after, ...rows.map((r) => r.id)); onLog(rows); }
      } catch { /* transient */ } finally { busy = false; }
    };
    catchup();
    const es = new EventSource(`/v1/pipeline/runs/${encodeURIComponent(runId)}/stage-logs/stream${token ? `?token=${encodeURIComponent(token)}` : ''}`);
    es.onopen = () => { onLive?.(); };
    es.onmessage = (ev) => {
      try { const row = JSON.parse(ev.data); after = Math.max(after, row.id); onLog([row]); } catch { /* ignore */ }
    };
    es.addEventListener('auth', () => { onAuthError?.(); es.close(); });
    es.onerror = () => {
      if (closed) return;
      es.close();
      if (!poll) poll = setInterval(catchup, 2000); // degrade to polling
    };
    return () => { closed = true; es.close(); if (poll) clearInterval(poll); };
  },
  runFiles: (id: string) => j<{ files: string[] }>(`/v1/pipeline/runs/${encodeURIComponent(id)}/files`),
  runFile: async (id: string, path: string) => {
    const headers: Record<string, string> = {};
    if (authToken) headers['Authorization'] = `Bearer ${authToken}`;
    const r = await fetch(`/v1/pipeline/runs/${encodeURIComponent(id)}/files/${path.split('/').map(encodeURIComponent).join('/')}`, { headers, credentials: 'include' });
    if (!r.ok) throw new Error(`HTTP ${r.status}`);
    return r.text();
  },
  packet: (id: string) => j<PacketBundle>(`/v1/pipeline/runs/${encodeURIComponent(id)}/packet`),
  savePacketRevision: (id: string, content: Record<string, unknown>, opts?: { packetId?: string; changeRequest?: string }) =>
    j<PacketRevision>(`/v1/pipeline/runs/${encodeURIComponent(id)}/packet/revisions`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ content, packet_id: opts?.packetId, change_request: opts?.changeRequest }),
    }),
  /** Authenticated download. Returns the server-selected filename; the caller
   * receives the same revision/hash/provenance/history as the review view. */
  downloadPacket: async (id: string, format: 'markdown' | 'json', revisionId?: string) => {
    const headers: Record<string, string> = {};
    if (authToken) headers['Authorization'] = `Bearer ${authToken}`;
    const query = new URLSearchParams({ format });
    if (revisionId) query.set('revision', revisionId);
    const r = await fetch(`/v1/pipeline/runs/${encodeURIComponent(id)}/packet/export?${query.toString()}`, { headers, credentials: 'include' });
    if (!r.ok) throw new Error((await r.json().catch(() => ({ error: r.statusText }))).error || `HTTP ${r.status}`);
    const blob = await r.blob();
    const disposition = r.headers.get('Content-Disposition') || '';
    const filename = /filename="([^"]+)"/.exec(disposition)?.[1] || `requirements-packet.${format === 'json' ? 'json' : 'md'}`;
    const url = URL.createObjectURL(blob);
    try {
      const anchor = document.createElement('a');
      anchor.href = url;
      anchor.download = filename;
      document.body.appendChild(anchor);
      anchor.click();
      anchor.remove();
    } finally {
      URL.revokeObjectURL(url);
    }
    return { filename };
  },
  plan: (id: string) => j<PlanBundle>(`/v1/pipeline/runs/${encodeURIComponent(id)}/plan`),
  savePlanRevision: (id: string, content: Record<string, unknown>, opts?: { planId?: string; changeRequest?: string }) =>
    j<PlanRevision>(`/v1/pipeline/runs/${encodeURIComponent(id)}/plan/revisions`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ content, plan_id: opts?.planId, change_request: opts?.changeRequest }),
    }),
  planTasks: (id: string, revisionId?: string, completed: string[] = []) => {
    const query = new URLSearchParams();
    if (revisionId) query.set('revision', revisionId);
    if (completed.length) query.set('completed', completed.join(','));
    return j<{ run_id: string; plan_id: string; revision_id: string; revision: number; content_hash: string; ready_task_ids: string[]; tasks: TaskReadiness[] }>(
      `/v1/pipeline/runs/${encodeURIComponent(id)}/plan/tasks?${query.toString()}`);
  },
  selectPlanTask: (id: string, request: { plan_revision_id: string; plan_content_hash: string; task_id: string; completed_task_ids?: string[] }) =>
    j<{ selection: { id: string; task_id: string; plan_revision_id: string; plan_content_hash: string; selected_by: string; created_at: string }; payload: Record<string, unknown>; replayed: boolean; task_selection_recorded?: boolean }>(
      `/v1/pipeline/runs/${encodeURIComponent(id)}/plan/select-task`, {
        method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ ...request, completed_task_ids: request.completed_task_ids ?? [] }),
      }),
  downloadPlan: async (id: string, format: 'markdown' | 'json', revisionId?: string) => {
    const headers: Record<string, string> = {};
    if (authToken) headers['Authorization'] = `Bearer ${authToken}`;
    const query = new URLSearchParams({ format });
    if (revisionId) query.set('revision', revisionId);
    const r = await fetch(`/v1/pipeline/runs/${encodeURIComponent(id)}/plan/export?${query.toString()}`, { headers, credentials: 'include' });
    if (!r.ok) throw new Error((await r.json().catch(() => ({ error: r.statusText }))).error || `HTTP ${r.status}`);
    const blob = await r.blob();
    const disposition = r.headers.get('Content-Disposition') || '';
    const filename = /filename="([^"]+)"/.exec(disposition)?.[1] || `task-plan.${format === 'json' ? 'json' : 'md'}`;
    const url = URL.createObjectURL(blob);
    try {
      const anchor = document.createElement('a');
      anchor.href = url;
      anchor.download = filename;
      document.body.appendChild(anchor);
      anchor.click();
      anchor.remove();
    } finally {
      URL.revokeObjectURL(url);
    }
    return { filename };
  },
  delivery: (id: string) => j<DeliveryBundle>(`/v1/pipeline/runs/${encodeURIComponent(id)}/delivery`),
  startDeliveryRelease: (id: string, request: { environment: DeliveryEnvironment; kind?: DeliveryKind; publication_id?: string; authorization_id?: string; source_release_id?: string; rollback_target_sha?: string }) =>
    j<{ release: DeliveryRelease; dispatched: boolean; replayed?: boolean; execution_authority: string; note?: string }>(
      `/v1/pipeline/runs/${encodeURIComponent(id)}/delivery/releases`, {
        method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(request),
      }),
  authorizeDelivery: (id: string, request: { publication_id: string; kind?: DeliveryKind; staging_release_id?: string; source_release_id?: string; rollback_target_sha?: string; idempotency_key: string; reason?: string }) =>
    j<{ decision: DeliveryDecision; replayed: boolean }>(
      `/v1/pipeline/runs/${encodeURIComponent(id)}/delivery/decisions`, {
        method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ ...request, decision: 'authorize' }),
      }),
  retryDeliveryDispatch: (id: string, releaseId: string) =>
    j<{ release: DeliveryRelease; dispatched: boolean; replayed: boolean; note?: string }>(
      `/v1/pipeline/runs/${encodeURIComponent(id)}/delivery/releases/${encodeURIComponent(releaseId)}/retry-dispatch`, {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ acknowledge_unknown_external_effect: true }),
      }),
  reconcileDelivery: (id: string, releaseId: string) =>
    j<{ reconciled: boolean; replayed?: boolean; error?: string | null; release: DeliveryRelease; event?: unknown; note?: string }>(
      `/v1/pipeline/runs/${encodeURIComponent(id)}/delivery/releases/${encodeURIComponent(releaseId)}/reconcile`, { method: 'POST' }),
  deleteChat: (chatId: string) => j<{ ok: boolean }>(`/v1/chats/${encodeURIComponent(chatId)}`, { method: 'DELETE' }),
  chat: (text: string, history: ChatMsg[], chatId: string) => {
    const messages = [
      ...history.slice(-20).map((m) => ({ role: m.who === 'you' ? 'user' : 'assistant', content: m.text })),
      { role: 'user', content: text },
    ];
    return j<{ choices: { message: { content: string } }[] }>('/v1/chat/completions', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ messages, stream: false, chat_id: chatId }) });
  },
};

/** Subscribe to SSE notifications; returns an unsubscribe fn. Auth via ?token= (EventSource cannot set headers). */
export function subscribeEvents(onEvent: () => void, onAuthError?: () => void): () => void {
  const token = authToken ?? '';
  const url = `/v1/pipeline/events${token ? `?token=${encodeURIComponent(token)}` : ''}`;
  let poll: ReturnType<typeof setInterval> | null = null;
  let closed = false;
  const es = new EventSource(url);
  let debounce: ReturnType<typeof setTimeout> | null = null;
  es.onmessage = () => {
    if (debounce) clearTimeout(debounce);
    debounce = setTimeout(onEvent, 400);
  };
  es.addEventListener('auth', () => { onAuthError?.(); es.close(); });
  es.onerror = () => {
    if (closed) return;
    es.close();
    if (!poll) poll = setInterval(onEvent, 8000);
  };
  return () => {
    closed = true;
    es.close();
    if (poll) clearInterval(poll);
    if (debounce) clearTimeout(debounce);
  };
}
