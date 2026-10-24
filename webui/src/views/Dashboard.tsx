import { useEffect, useRef, useState, type ReactNode } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { api } from '../api';
import type { ChatMsg, Readiness, TjRun } from '../types';
import { Dot, StatusChip, type Nav } from '../App';

interface ChatThread {
  id: string;
  title: string;
  messages: ChatMsg[];
}

function loadChats(key: string): ChatThread[] {
  try {
    const raw = localStorage.getItem(key);
    if (raw) {
      const parsed = JSON.parse(raw) as ChatThread[];
      // Keep the user-scoped storage key (`tj-chats-v2:<user>`) and normalize
      // the bounded legacy persona token. Old chats must not be lost or leak
      // across accounts.
      return parsed.map((thread) => ({
        ...thread,
        messages: (thread.messages || []).map((message) => ({
          ...message,
          who: message.who === 'you' ? 'you' : 'chief',
        })),
      }));
    }
  } catch { /* corrupt storage — reset */ }
  return [{ id: crypto.randomUUID(), title: 'New chat', messages: [] }];
}

function GateRow({ run, nav }: { run: TjRun; nav: Nav }) {
  return (
    <button className="rowbtn" onClick={() => nav.openRun(run.id)}>
      <Dot status="waiting_for_gate" />
      <span style={{ flex: 1, minWidth: 0 }}>
        <span className="rowpath">{run.description}</span>
        <span className="rowtitle">Gate</span>
        <span className="tiny dim"> — {run.gate_state || 'approval required'}</span>
      </span>
      <span className="chip c-wait">approval needed</span>
    </button>
  );
}

function RunRow({ run, nav }: { run: TjRun; nav: Nav }) {
  return (
    <button className="rowbtn" onClick={() => nav.openRun(run.id)}>
      <Dot status={run.status} />
      <span style={{ flex: 1, minWidth: 0 }}>
        <span className="rowtitle">{run.description || run.id}</span>
        <span className="tiny dim" style={{ display: 'block' }}>
          {new Date(run.created_at).toLocaleString()} · {run.status.replaceAll('_', ' ')}
        </span>
      </span>
      <StatusChip status={run.status} />
    </button>
  );
}

function Step({ done, title, detail, action }: { done: boolean; title: string; detail: string; action?: ReactNode }) {
  return (
    <div className="setup-step" role="listitem">
      <span className={`setup-mark ${done ? 'ok' : 'todo'}`} aria-hidden="true">{done ? '✓' : '•'}</span>
      <span style={{ flex: 1, minWidth: 0 }}>
        <span style={{ display: 'block', fontWeight: 600 }}>{title}</span>
        <span className="tiny dim" style={{ display: 'block' }}>{detail}</span>
        {action}
      </span>
    </div>
  );
}

function FirstRunPanel({ nav, readiness, projects }: { nav: Nav; readiness: Readiness; projects: number }) {
  const qc = useQueryClient();
  const [name, setName] = useState('');
  const [showForm, setShowForm] = useState(projects === 0);
  const catalog = useQuery({ queryKey: ['profiles'], queryFn: api.profiles });
  const entries = catalog.data?.profiles || [];
  const [catalogId, setCatalogId] = useState('');
  const selected = entries.find((entry) => entry.catalog_id === catalogId) || entries[0];
  const create = useMutation({
    mutationFn: () => api.createProject(name.trim(), { catalog_id: selected?.catalog_id }),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['projects'] });
      qc.invalidateQueries({ queryKey: ['lanes'] });
      qc.invalidateQueries({ queryKey: ['runs'] });
      qc.invalidateQueries({ queryKey: ['readiness'] });
      setName('');
      setShowForm(false);
    },
  });
  const askChief = (prompt: string) => {
    window.dispatchEvent(new CustomEvent('tj-chief-prompt', { detail: prompt }));
    document.getElementById('chief-panel')?.scrollIntoView({ behavior: 'smooth', block: 'start' });
  };
  const provider = readiness.provider;
  const providerDetail = provider.ready
    ? `Ready with ${provider.provider_id || 'provider'} / ${provider.model || 'model'}.`
    : `Missing credential. Set ${provider.credential_env || 'the provider API-key environment variable'} and restart TurboJet.`;
  return (
    <section className="card pad setup-card" aria-labelledby="setup-title" role="region">
      <div className="setup-head">
        <span>
          <h3 id="setup-title" style={{ margin: 0 }}>Set up TurboJet</h3>
          <span className="tiny dim">Finish these steps once. Chief stays your main contact; direct controls stay available.</span>
        </span>
        <span className={`chip ${readiness.status === 'ready' ? 'c-done' : readiness.status === 'blocked' ? 'c-fail' : 'c-wait'}`}>
          {readiness.status}
        </span>
      </div>
      <div role="list" aria-label="Setup checklist">
        <Step done={readiness.first_run.admin_exists} title="Administrator account" detail="The first account becomes the administrator and can add members later." />
        <Step done={provider.ready} title="Supported model provider" detail={providerDetail} action={
          !provider.ready && provider.credential_env
            ? <code className="tiny mono" style={{ display: 'block', marginTop: 3 }}>{provider.credential_env}=…</code>
            : undefined
        } />
        <Step done={projects > 0} title="Workflow project" detail={entries.length ? 'Choose a registered workflow, then create a project with an exact version lane.' : 'No workflow installed yet. Configure a workflow source or import a workflow before creating a project.'} action={
          <span style={{ display: 'inline-flex', gap: 6, marginTop: 4, flexWrap: 'wrap' }}>
            {!showForm && <button className="btn btn-sm" type="button" disabled={!entries.length} onClick={() => setShowForm(true)}>Create project</button>}
            <button className="btn btn-sm" type="button" onClick={() => askChief('Create a project from a registered workflow and help me describe what I need.')}>Ask Chief</button>
          </span>
        } />
        <Step done={false} title="Start with Chief or the direct form" detail="Describe the work, answer any questions, then review the exact output at the gate." action={
          showForm && entries.length ? (
            <form className="setup-form" onSubmit={(e) => { e.preventDefault(); if (name.trim() && selected) create.mutate(); }}>
              <label className="tiny" htmlFor="setup-project" style={{ display: 'block', marginBottom: 3 }}>Project name</label>
              <div style={{ display: 'flex', gap: 6, flexWrap: 'wrap' }}>
                <input id="setup-project" value={name} onChange={(e) => setName(e.target.value)} placeholder="e.g. Review workflow" style={{ flex: 1, minWidth: 150 }} />
                <select aria-label="Workflow" value={selected?.catalog_id || ''} onChange={(e) => setCatalogId(e.target.value)} style={{ minWidth: 170 }}>
                  {entries.map((entry) => <option key={entry.catalog_id} value={entry.catalog_id}>{entry.label}</option>)}
                </select>
                <button className="btn btn-go btn-sm" type="submit" disabled={!name.trim() || !selected || create.isPending}>Create project</button>
              </div>
            </form>
          ) : undefined
        } />
      </div>
      {create.isError && <p className="tiny" style={{ color: 'var(--fail)', margin: '6px 0 0' }}>{(create.error as Error).message}</p>}
      <div className="setup-actions">
        <button className="btn btn-go btn-sm" type="button" disabled={!entries.length} onClick={() => askChief('Start a run. Ask me what work I need done.')}>Start with Chief</button>
        <button className="btn btn-sm" type="button" onClick={() => nav.setView('runs')}>Go to Runs</button>
        <button className="btn btn-sm" type="button" onClick={() => nav.setView('workflows')}>Inspect workflows</button>
        <span className="tiny dim">Retention: activity is kept {readiness.activity_retention_days} days; artifacts/packets are not pruned.</span>
      </div>
    </section>
  );
}

function NewRunCard({ nav }: { nav: Nav }) {
  const qc = useQueryClient();
  const [text, setText] = useState('');
  const [showNewProject, setShowNewProject] = useState(false);
  const [projName, setProjName] = useState('');
  const [projRepo, setProjRepo] = useState('');
  const [projCatalogId, setProjCatalogId] = useState('');
  const projects = useQuery({ queryKey: ['projects'], queryFn: api.projects });
  const laneList = useQuery({ queryKey: ['lanes'], queryFn: api.lanes });
  const profileList = useQuery({ queryKey: ['profiles'], queryFn: api.profiles });
  const profiles = profileList.data?.profiles || [];
  const explicitProfile = profiles.find((p) => p.catalog_id === projCatalogId);
  const declaredDefault = profiles.find((p) => p.default);
  const selectedProfile = explicitProfile
    ?? (profiles.length === 1 ? profiles[0] : declaredDefault);
  const needsCatalogChoice = profiles.length > 1 && !selectedProfile;
  const list = projects.data || [];
  const lanes = laneList.data?.lanes || [];
  const [projectId, setProjectId] = useState('');
  const effectiveProject = projectId || list[0]?.id || '';
  const projectLanes = lanes.filter((l) => l.project_id === effectiveProject);
  const [laneId, setLaneId] = useState('');
  const [projectNotice, setProjectNotice] = useState<string | null>(null);
  const effectiveLane = laneId && projectLanes.some((l) => l.id === laneId) ? laneId : projectLanes[0]?.id || '';
  const create = useMutation({
    mutationFn: () => api.createRun(text.trim(), effectiveProject, effectiveLane),
    onSuccess: (run) => {
      qc.invalidateQueries({ queryKey: ['runs'] });
      setText('');
      nav.openRun(run.id);
    },
  });
  const createProject = useMutation({
    mutationFn: () => api.createProject(projName.trim(), selectedProfile?.repository_free === false
      ? { catalog_id: selectedProfile.catalog_id, repo_url: projRepo.trim() || undefined }
      : { catalog_id: selectedProfile?.catalog_id }),
    onSuccess: (d) => {
      qc.invalidateQueries({ queryKey: ['projects'] });
      qc.invalidateQueries({ queryKey: ['workflows'] });
      setProjectId(d.project.id);
      setProjectNotice(d.project.provisioning_state === 'failed'
        ? `Project retained, but repository provisioning failed: ${d.project.provisioning_error || 'configuration required'}. Configure the project key and verified host identity, then retry provisioning.`
        : null);
      setProjName(''); setProjRepo(''); setProjCatalogId(''); setShowNewProject(false);
    },
  });

  return (
    <div className="card pad" id="new-run-card" style={{ marginBottom: 14 }}>
      <h3 style={{ margin: '0 0 8px' }}>Start a run</h3>
      <p className="tiny dim" style={{ margin: '0 0 8px' }}>Every run executes on a workflow (project + lane).</p>
      <div style={{ display: 'flex', gap: 7, flexWrap: 'wrap', marginBottom: 7 }}>
        <select value={effectiveProject} onChange={(e) => { setProjectId(e.target.value); setLaneId(''); }} style={{ flex: 2, minWidth: 140 }} aria-label="Project">
          {!list.length && <option value="">No projects yet…</option>}
          {list.map((p) => <option key={p.id} value={p.id}>{p.name}{p.provisioning_state === 'failed' ? ' — provisioning failed' : ''}</option>)}
        </select>
        <select value={effectiveLane} onChange={(e) => setLaneId(e.target.value)} style={{ flex: 1, minWidth: 110 }} aria-label="Workflow">
          {!projectLanes.length && <option value="">No workflow</option>}
          {projectLanes.map((l) => <option key={l.id} value={l.id}>{l.name}</option>)}
        </select>
        <button className="btn" type="button" onClick={() => setShowNewProject(!showNewProject)}>+ Project</button>
      </div>
      {showNewProject && (
        <div style={{ display: 'flex', gap: 7, flexWrap: 'wrap', marginBottom: 7 }}>
          <input style={{ flex: 1, minWidth: 120 }} value={projName} placeholder="Project name" onChange={(e) => setProjName(e.target.value)} />
          <select aria-label="Workflow catalog" value={selectedProfile?.catalog_id || ''} onChange={(e) => setProjCatalogId(e.target.value)} style={{ minWidth: 190 }} disabled={!profiles.length}>
            {!profiles.length && <option value="">No workflow installed</option>}
            {needsCatalogChoice && <option value="">Choose a workflow</option>}
            {profiles.map((p) => (
              <option key={p.catalog_id} value={p.catalog_id}>{p.label} ({p.maturity})</option>
            ))}
          </select>
          {selectedProfile && <span className="tiny dim" style={{ flexBasis: '100%' }}>{selectedProfile.description}</span>}
          {selectedProfile?.repository_free === false && (
            <input style={{ flex: 2, minWidth: 160 }} value={projRepo} placeholder="Git repo URL" onChange={(e) => setProjRepo(e.target.value)} />
          )}
          <button className="btn" type="button" disabled={!projName.trim() || !selectedProfile || createProject.isPending} onClick={() => createProject.mutate()}>Create</button>
        </div>
      )}
      {createProject.isError && <p className="tiny" style={{ color: 'var(--fail)', margin: '0 0 6px' }}>{(createProject.error as Error).message}</p>}
      {projectNotice && <p className="tiny" role="status" style={{ color: 'var(--wait)', margin: '0 0 6px' }}>{projectNotice}</p>}
      <form style={{ display: 'flex', gap: 7 }} onSubmit={(e) => { e.preventDefault(); if (text.trim() && effectiveProject && effectiveLane) create.mutate(); }}>
        <input style={{ flex: 1 }} value={text} placeholder="Describe the task — e.g. “Add a /healthz endpoint”" onChange={(e) => setText(e.target.value)} />
        <button className="btn" type="submit" disabled={create.isPending || !text.trim() || !effectiveProject || !effectiveLane}>Run</button>
      </form>
      {create.isError && <p className="tiny" style={{ color: 'var(--fail)', margin: '6px 0 0' }}>{(create.error as Error).message}</p>}
    </div>
  );
}

function ChiefChat({ userId }: { userId: string }) {
  const chatsKey = `tj-chats-v2:${userId}`;
  const [chats, setChats] = useState<ChatThread[]>(() => loadChats(chatsKey));
  const [activeId, setActiveId] = useState(() => chats[0]?.id ?? '');
  const [text, setText] = useState('');
  const bottomRef = useRef<HTMLDivElement>(null);
  const active = chats.find((c) => c.id === activeId) ?? chats[0];

  useEffect(() => { localStorage.setItem(chatsKey, JSON.stringify(chats)); }, [chats, chatsKey]);
  useEffect(() => { bottomRef.current?.scrollIntoView?.({ behavior: 'smooth' }); }, [active?.messages.length]);
  useEffect(() => {
    const receive = (event: Event) => {
      const prompt = (event as CustomEvent<string>).detail;
      if (typeof prompt === 'string') setText(prompt);
    };
    window.addEventListener('tj-chief-prompt', receive);
    return () => window.removeEventListener('tj-chief-prompt', receive);
  }, []);

  const ask = useMutation({
    mutationFn: (q: string) => api.chat(q, active.messages, active.id),
    onSuccess: (d) => {
      const reply = d.choices?.[0]?.message?.content || '(empty)';
      setChats((cs) => cs.map((c) => c.id === active.id
        ? { ...c, messages: [...c.messages, { who: 'chief' as const, text: reply }] }
        : c));
    },
    onError: (e) => setChats((cs) => cs.map((c) => c.id === active.id
      ? { ...c, messages: [...c.messages, { who: 'chief' as const, text: `(unavailable: ${(e as Error).message})` }] }
      : c)),
  });

  const send = () => {
    if (!text.trim() || ask.isPending) return;
    const q = text.trim();
    setChats((cs) => cs.map((c) => c.id === active.id
      ? { ...c, title: c.messages.length ? c.title : q.slice(0, 40), messages: [...c.messages, { who: 'you' as const, text: q }] }
      : c));
    setText('');
    ask.mutate(q);
  };

  const newChat = () => {
    const c: ChatThread = { id: crypto.randomUUID(), title: 'New chat', messages: [] };
    setChats((cs) => [c, ...cs]);
    setActiveId(c.id);
  };

  const deleteChat = async (id: string) => {
    await api.deleteChat(id).catch(() => undefined);
    setChats((cs) => {
      const next = cs.filter((c) => c.id !== id);
      const ensured = next.length ? next : [{ id: crypto.randomUUID(), title: 'New chat', messages: [] }];
      if (id === activeId) setActiveId(ensured[0].id);
      return ensured;
    });
  };

  return (
    <div className="card pad" id="chief-panel">
      <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 8 }}>
        <h3 style={{ margin: 0 }}>Ask Chief</h3>
        <span className="tiny dim">your point of contact for everything TurboJet</span>
        <button className="btn btn-sm" style={{ marginLeft: 'auto' }} onClick={newChat}>+ New chat</button>
      </div>
      <div className="chief-prompts" aria-label="Chief quick prompts">
        {[
          'Start a run and ask me what work to do.',
          'What is waiting for my review?',
          'Explain the exact output waiting for my review.',
        ].map((prompt) => (
          <button key={prompt} type="button" className="fchip" onClick={() => setText(prompt)}>{prompt}</button>
        ))}
      </div>

      {chats.length > 1 && (
        <div style={{ display: 'flex', gap: 6, flexWrap: 'wrap', marginBottom: 8 }}>
          {chats.map((c) => (
            <span key={c.id} style={{ display: 'inline-flex', alignItems: 'center', gap: 4 }}>
              <button className={`fchip ${c.id === active.id ? 'on' : ''}`} aria-pressed={c.id === active.id} onClick={() => setActiveId(c.id)}>
                {c.title.slice(0, 24)}
              </button>
              <button className="tiny dim" style={{ border: 0, background: 'none', cursor: 'pointer' }} title="Delete chat" onClick={() => deleteChat(c.id)}>✕</button>
            </span>
          ))}
        </div>
      )}

      <div style={{ maxHeight: 320, overflowY: 'auto' }}>
        {active.messages.map((m, i) => (
          <div key={i} className={`msg ${m.who === 'you' ? 'you' : 'ai'}`}><div className="who">{m.who === 'you' ? 'You' : 'Chief'}</div>{m.text}</div>
        ))}
        {ask.isPending && <div className="msg ai"><div className="who">Chief</div>…thinking</div>}
        <div ref={bottomRef} />
      </div>
      <form className="composer" style={{ display: 'flex', gap: 7, marginTop: 9 }} onSubmit={(e) => { e.preventDefault(); send(); }}>
        <input style={{ flex: 1 }} value={text} placeholder="Ask Chief to run, inspect, or explain anything…" onChange={(e) => setText(e.target.value)} />
        <button className="btn" type="submit" disabled={ask.isPending || !text.trim()}>Send</button>
      </form>
    </div>
  );
}

export function Dashboard({ nav, userId }: { nav: Nav; userId: string }) {
  const runs = useQuery({ queryKey: ['runs'], queryFn: api.runs });
  const readiness = useQuery({ queryKey: ['readiness'], queryFn: api.readiness, retry: false });
  const projects = useQuery({ queryKey: ['projects'], queryFn: api.projects });
  const list = runs.data || [];
  const waiting = list.filter((r) => r.status === 'waiting_for_gate');
  const inFlight = list.filter((r) => r.status === 'running');
  const attention = list.filter((r) => ['interrupted', 'failed'].includes(r.status));
  const finished = list.filter((r) => ['completed', 'failed', 'cancelled'].includes(r.status))
    .sort((a, b) => b.updated_at.localeCompare(a.updated_at)).slice(0, 5);
  const setupRequired = Boolean(readiness.data?.first_run.setup_required);

  return (
    <div className="inbox-grid">
      <div>
        {setupRequired && readiness.data && (
          <FirstRunPanel nav={nav} readiness={readiness.data} projects={(projects.data || []).length} />
        )}
        <div className="card">
          <div className="pad" style={{ borderBottom: '1px solid var(--rule)' }}>
            <h3 style={{ margin: 0 }}>Waiting on you</h3>
            <span className="tiny dim">Gates across every run.</span>
          </div>
          {waiting.length ? waiting.map((r) => <GateRow key={r.id} run={r} nav={nav} />)
            : <div className="pad tiny dim">Nothing to decide. The queue is empty.</div>}
          {attention.length > 0 && <div className="sechead" style={{ margin: '8px 14px' }}>Needs attention <span className="tiny dim">{attention.length}</span></div>}
          {attention.map((r) => (
            <div key={r.id}>
              <RunRow run={r} nav={nav} />
              <p className="tiny dim" style={{ margin: '-4px 14px 8px 32px' }}>
                {r.status === 'interrupted'
                  ? 'Interrupted by a restart. Open the run, inspect preserved questions/artifacts, then use Retry for a new attempt.'
                  : 'Failed visibly. Open the run for the exact stage error and budget/activity context.'}
              </p>
            </div>
          ))}
          <div className="sechead" style={{ margin: '8px 14px' }}>In flight <span className="tiny dim">{inFlight.length}</span></div>
          {inFlight.map((r) => <RunRow key={r.id} run={r} nav={nav} />)}
          {!inFlight.length && <div className="pad tiny dim">No runs in flight.</div>}
        </div>
      </div>
      <div>
        <NewRunCard nav={nav} />
        <div className="card" style={{ marginBottom: 14 }}>
          <div className="pad" style={{ borderBottom: '1px solid var(--rule)' }}><h3 style={{ margin: 0 }}>Recently finished</h3></div>
          {finished.map((r) => <RunRow key={r.id} run={r} nav={nav} />)}
          {!finished.length && <div className="pad tiny dim">No finished runs yet.</div>}
        </div>
        <ChiefChat key={userId} userId={userId} />
      </div>
    </div>
  );
}
