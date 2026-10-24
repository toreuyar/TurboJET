import { useEffect, useState } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { api, getToken, setToken, subscribeEvents } from './api';
import type { Project, RunTree, User } from './types';
import { Dashboard } from './views/Dashboard';
import { RunsList, RunDetail } from './views/Runs';
import { Workflows, WorkflowDetailPage } from './views/Workflows';

export type View = 'dashboard' | 'runs' | 'run' | 'workflows' | 'workflow';

export interface Nav {
  view: View;
  projectId: string | null;
  runId: string | null;
  wfId: string | null;
  setView: (v: View) => void;
  openRun: (id: string) => void;
  openWorkflow: (id: string) => void;
  setProject: (id: string | null) => void;
}

function readHash(): { view: View; runId: string | null; wfId: string | null } {
  const h = new URLSearchParams(location.hash.replace(/^#/, ''));
  const v = (h.get('view') || 'dashboard') as View;
  return { view: ['dashboard', 'runs', 'run', 'workflows', 'workflow'].includes(v) ? v : 'dashboard', runId: h.get('run'), wfId: h.get('wf') };
}

export function StatusChip({ status }: { status: string }) {
  const map: Record<string, [string, string]> = {
    running: ['c-run', 'Running'], waiting_for_gate: ['c-wait', 'Waiting at gate'], waiting_for_input: ['c-wait', 'Waiting for input'],
    completed: ['c-done', 'Completed'], done: ['c-done', 'Done'], failed: ['c-fail', 'Failed'], timed_out: ['c-fail', 'Timed out'], interrupted: ['c-wait', 'Interrupted — retry required'],
    cancelled: ['c-fail', 'Cancelled'], pending: ['c-idle', 'Not started'],
  };
  const [cls, label] = map[status] || ['c-idle', status];
  return <span className={`chip ${cls}`}>{label}</span>;
}

export function Dot({ status }: { status: string }) {
  const map: Record<string, string> = {
    running: 'd-run', waiting_for_gate: 'd-wait', waiting_for_input: 'd-wait',
    completed: 'd-done', done: 'd-done', failed: 'd-fail', timed_out: 'd-fail', cancelled: 'd-fail', interrupted: 'd-wait', pending: 'd-idle',
  };
  return <span className={`dot ${map[status] || 'd-idle'}`} />;
}

function LoginPanel({ onDone }: { onDone: () => void }) {
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [mode, setMode] = useState<'login' | 'register'>('login');
  const [busy, setBusy] = useState(false);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true); setError(null);
    try {
      const fn = mode === 'login' ? api.login : api.register;
      const res = await fn(username.trim(), password);
      setToken(res.token);
      onDone();
    } catch (err) {
      setError((err as Error).message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="main" style={{ maxWidth: 460 }}>
      <h2>TurboJet webui</h2>
      <p className="dim tiny" style={{ margin: '0 0 14px' }}>Sign in with your TurboJet account.</p>
      <form className="card pad" onSubmit={submit}>
        <div className="field">
          <label htmlFor="u">Username</label>
          <input id="u" value={username} onChange={(e) => setUsername(e.target.value)} autoComplete="username" />
        </div>
        <div className="field">
          <label htmlFor="p">Password</label>
          <input id="p" type="password" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="current-password" />
        </div>
        {error && <p className="tiny" style={{ color: 'var(--fail)', margin: '0 0 10px' }}>{error}</p>}
        <div style={{ display: 'flex', gap: 8 }}>
          <button className="btn btn-go" type="submit" disabled={busy}>
            {mode === 'login' ? 'Sign in' : 'Create first account'}
          </button>
          <button
            type="button"
            className="btn"
            onClick={() => { setMode(mode === 'login' ? 'register' : 'login'); setError(null); }}
            title="Registration only works while no accounts exist — creates the first admin"
          >
            {mode === 'login' ? 'First run? Create account' : 'Back to sign in'}
          </button>
        </div>
        <p className="tiny dim" style={{ margin: '10px 0 0' }}>
          Local accounts, stored in TurboJet. Behind the proxy this page is only reachable by authenticated users.
        </p>
      </form>
    </div>
  );
}

export default function App() {
  const qc = useQueryClient();
  const [authed, setAuthed] = useState<boolean>(!!getToken());
  const [hash, setHash] = useState(readHash());
  const [projectId, setProjectId] = useState<string | null>(null);
  const [fullWidth, setFullWidth] = useState(localStorage.getItem('tj-width') === 'full');
  const [theme, setTheme] = useState(localStorage.getItem('tj-theme') || 'auto');

  useEffect(() => {
    const expired = () => { setToken(null); setAuthed(false); setProjectId(null); qc.clear(); };
    window.addEventListener('tj-auth-expired', expired);
    return () => window.removeEventListener('tj-auth-expired', expired);
  }, [qc]);

  useEffect(() => {
    const onHash = () => setHash(readHash());
    window.addEventListener('hashchange', onHash);
    return () => window.removeEventListener('hashchange', onHash);
  }, []);

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    localStorage.setItem('tj-theme', theme);
  }, [theme]);

  useEffect(() => {
    localStorage.setItem('tj-width', fullWidth ? 'full' : 'comfy');
  }, [fullWidth]);

  const me = useQuery({ queryKey: ['me'], queryFn: api.me, enabled: authed, retry: false,
    meta: { onError: (e: Error & { status?: number }) => { if (e.status === 401) { setToken(null); setAuthed(false); } } } });
  const projects = useQuery({ queryKey: ['projects'], queryFn: api.projects, enabled: authed });
  const runsQ = useQuery({ queryKey: ['runs'], queryFn: api.runs, enabled: authed });

  useEffect(() => {
    if (!authed) return;
    const unsub = subscribeEvents(
      () => { qc.invalidateQueries({ queryKey: ['runs'] }); qc.invalidateQueries({ queryKey: ['tree'] }); qc.invalidateQueries({ queryKey: ['decisions'] }); qc.invalidateQueries({ queryKey: ['events'] }); qc.invalidateQueries({ queryKey: ['artifacts'] }); qc.invalidateQueries({ queryKey: ['delivery'] }); },
      () => { setToken(null); setAuthed(false); setProjectId(null); qc.clear(); },
    );
    return unsub;
  }, [authed, qc]);

  if (!authed) {
    return <LoginPanel onDone={() => { setAuthed(true); qc.invalidateQueries(); }} />;
  }

  const user: User | undefined = me.data;
  if (!user) return <p className="tiny dim">{me.error ? 'Unable to verify session. Please reload or sign in again.' : 'Loading session…'}</p>;
  const projList: Project[] = projects.data || [];
  const nav: Nav = {
    view: hash.view,
    projectId,
    runId: hash.runId,
    wfId: hash.wfId,
    setView: (v) => { location.hash = v === 'run' && hash.runId ? `#view=run&run=${hash.runId}` : `#view=${v}`; },
    openRun: (id) => { location.hash = `#view=run&run=${id}`; setHash({ view: 'run', runId: id, wfId: null }); },
    openWorkflow: (id) => { location.hash = `#view=workflow&wf=${id}`; setHash({ view: 'workflow', runId: null, wfId: id }); },
    setProject: setProjectId,
  };

  const tabs: [View, string][] = [['dashboard', 'Dashboard'], ['runs', 'Runs'], ['workflows', 'Workflows']];
  const waitingCount = (runsQ.data || []).filter((r) => r.status === 'waiting_for_gate').length;

  return (
    <>
      <div className="topbar">
        <div className="brand">TurboJet <span>/ webui</span></div>
        <nav className="tabs" aria-label="Views">
          {tabs.map(([v, label]) => (
            <button key={v} className="tab" aria-current={hash.view === v || (v === 'runs' && hash.view === 'run') || (v === 'workflows' && hash.view === 'workflow')} onClick={() => nav.setView(v)}>{label}</button>
          ))}
        </nav>
        <div className="topright">
          <select aria-label="Project" value={projectId ?? ''} onChange={(e) => setProjectId(e.target.value || null)} title="Scope: Runs filter by project; Dashboard is cross-project">
            <option value="">All projects</option>
            {projList.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}
          </select>
          <span className="pill">{waitingCount} waiting</span>
          <button className="tbtn" onClick={() => setTheme(theme === 'auto' ? 'light' : theme === 'light' ? 'dark' : 'auto')}>
            {theme === 'auto' ? 'Auto' : theme === 'light' ? 'Light' : 'Dark'}
          </button>
          <button className={`tbtn ${fullWidth ? 'on' : ''}`} onClick={() => setFullWidth(!fullWidth)}>
            {fullWidth ? 'Comfortable' : 'Full width'}
          </button>
          {user && <span className="tiny dim">{user.display_name || user.username}{user.is_admin ? ' · admin' : ''}</span>}
          <button className="tbtn" onClick={async () => { try { await api.logout(); } catch { /* ignore */ } setToken(null); setAuthed(false); qc.clear(); }}>Sign out</button>
        </div>
      </div>
      <div className={`main ${fullWidth ? 'full' : ''}`} key={hash.view + (hash.runId || '') + (hash.wfId || '')}>
        {hash.view === 'dashboard' && <Dashboard nav={nav} userId={user.id} />}
        {hash.view === 'runs' && <RunsList nav={nav} />}
        {hash.view === 'run' && hash.runId && <RunDetail nav={nav} runId={hash.runId} />}
        {hash.view === 'run' && !hash.runId && <p className="tiny dim">No run selected — pick one from Runs.</p>}
        {hash.view === 'workflows' && <Workflows nav={nav} />}
        {hash.view === 'workflow' && hash.wfId && <WorkflowDetailPage nav={nav} wfId={hash.wfId} />}
        {hash.view === 'workflow' && !hash.wfId && <p className="tiny dim">No workflow selected — pick one from Workflows.</p>}
      </div>
    </>
  );
}

export function useRunTree(runId: string | null) {
  return useQuery<RunTree>({
    queryKey: ['tree', runId],
    queryFn: () => api.tree(runId!),
    enabled: !!runId,
  });
}
