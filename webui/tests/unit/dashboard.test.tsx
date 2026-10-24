import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { Dashboard } from '../../src/views/Dashboard';
import { api } from '../../src/api';
import type { Nav } from '../../src/App';
import type { Readiness } from '../../src/types';

vi.mock('../../src/api', () => ({
  api: {
    runs: vi.fn(),
    projects: vi.fn(),
    readiness: vi.fn(),
    profiles: vi.fn(),
    lanes: vi.fn(),
    createProject: vi.fn(),
    createRun: vi.fn(),
    chat: vi.fn(),
    deleteChat: vi.fn(),
  },
}));

const nav: Nav = {
  view: 'dashboard', projectId: null, runId: null, wfId: null,
  setView: vi.fn(), openRun: vi.fn(), openWorkflow: vi.fn(), setProject: vi.fn(),
};

const blocked: Readiness = {
  status: 'blocked',
  version: '1.0.0',
  checks: [],
  first_run: { admin_exists: true, project_count: 0, setup_required: true, next_step: 'Create a project' },
  provider: { agent_id: 'chief', provider_id: 'zai', model: 'glm-5.1', credential_env: 'ZAI_API_KEY', ready: false },
  activity_retention_days: 30,
};

function withClient(el: React.ReactElement) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={qc}>{el}</QueryClientProvider>);
}

describe('first-run setup', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    localStorage.clear();
    vi.mocked(api.runs).mockResolvedValue([]);
    vi.mocked(api.projects).mockResolvedValue([]);
    vi.mocked(api.lanes).mockResolvedValue({ lanes: [] });
    vi.mocked(api.readiness).mockResolvedValue(blocked);
    vi.mocked(api.profiles).mockResolvedValue({ profiles: [
      { id: 'flow-a', catalog_id: 'flow-a', label: 'Flow A', description: 'Synthetic repository-free flow', workflow_id: 'wf-a', smg_path: 'fixture-a.json', maturity: 'supported', repository_free: true, gate_binding: 'artifact', default: false, versions: [] },
      { id: 'flow-b', catalog_id: 'flow-b', label: 'Flow B', description: 'Synthetic experimental flow', workflow_id: 'wf-b', smg_path: 'fixture-b.json', maturity: 'experimental', repository_free: true, gate_binding: 'artifact', default: false, versions: [] },
      { id: 'flow-c', catalog_id: 'flow-c', label: 'Repo Flow', description: 'Synthetic repository-backed flow', workflow_id: 'wf-c', smg_path: 'fixture-c.json', maturity: 'experimental', repository_free: false, gate_binding: 'artifact', default: false, versions: [] },
    ] });
  });

  it('shows actionable provider/config steps and creates a project', async () => {
    vi.mocked(api.createProject).mockResolvedValue({
      project: { id: 'p1', name: 'Fixture project', repo_url: null, base_branch: 'main', workspace_path: null, profile: 'flow-a' as never, created_at: '', lanes: [] },
      lane: null,
    });
    withClient(<Dashboard nav={nav} userId="u1" />);
    expect(await screen.findByRole('heading', { name: /Set up/ })).toBeInTheDocument();
    expect(screen.getByText(/Missing credential/)).toBeInTheDocument();
    expect(screen.getByText('ZAI_API_KEY=…')).toBeInTheDocument();
    fireEvent.click(await screen.findByRole('button', { name: '+ Project' }));
    fireEvent.change(await screen.findByLabelText('Workflow catalog'), { target: { value: 'flow-a' } });
    fireEvent.change(screen.getByLabelText('Project name'), { target: { value: 'Fixture workflow' } });
    fireEvent.click(screen.getByRole('button', { name: /^Create / }));
    await waitFor(() => expect(api.createProject).toHaveBeenCalledWith('Fixture workflow', expect.objectContaining({ catalog_id: expect.any(String) })));
  });

  it('requires an explicit workflow choice when several catalogs have no default', async () => {
    withClient(<Dashboard nav={nav} userId="u1" />);
    fireEvent.click(await screen.findByRole('button', { name: '+ Project' }));
    const select = await screen.findByLabelText('Workflow catalog');
    expect((select as HTMLSelectElement).value).toBe('');
    expect(screen.getByRole('button', { name: 'Create' })).toBeDisabled();
    fireEvent.change(screen.getByPlaceholderText('Project name'), { target: { value: 'Synthetic project' } });
    fireEvent.change(select, { target: { value: 'flow-b' } });
    expect(screen.getByRole('button', { name: 'Create' })).toBeEnabled();
  });

  it('discovers arbitrary catalog entries from the registry picker', async () => {
    vi.mocked(api.createProject).mockResolvedValue({
      project: { id: 'p_flow', name: 'Flow B', repo_url: null, base_branch: 'main', workspace_path: null, profile: 'flow-b' as never, created_at: '', lanes: [] },
      lane: null,
    });
    withClient(<Dashboard nav={nav} userId="u1" />);
    fireEvent.click(await screen.findByRole('button', { name: '+ Project' }));
    const select = await screen.findByLabelText('Workflow catalog');
    const labels = Array.from((select as HTMLSelectElement).options).map((o) => o.textContent);
    expect(labels.some((label) => label?.includes('Flow A'))).toBe(true);
    expect(labels.some((label) => label?.includes('Flow B'))).toBe(true);
    expect(labels.some((label) => label?.includes('Repo Flow'))).toBe(true);
    fireEvent.change(screen.getByPlaceholderText('Project name'), { target: { value: 'Synthetic project' } });
    fireEvent.change(select, { target: { value: 'flow-b' } });
    fireEvent.click(screen.getByRole('button', { name: 'Create' }));
    await waitFor(() => expect(api.createProject).toHaveBeenCalledWith('Synthetic project', expect.objectContaining({ catalog_id: 'flow-b' })));
  });

  it('empty registry explains how to install a workflow and disables creation', async () => {
    vi.mocked(api.profiles).mockResolvedValue({ profiles: [], catalog_format_version: 1 } as never);
    withClient(<Dashboard nav={nav} userId="u1" />);
    expect(await screen.findByText(/No workflow installed yet/)).toBeInTheDocument();
    expect(screen.queryByLabelText('Workflow catalog')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /^Create/ })).not.toBeInTheDocument();
  });

  it('Chief quick prompts populate the composer without inventing an answer', async () => {
    vi.mocked(api.readiness).mockResolvedValue({ ...blocked, status: 'ready', provider: { ...blocked.provider, ready: true } });
    withClient(<Dashboard nav={nav} userId="u1" />);
    const prompt = await screen.findByRole('button', { name: /Start .* and ask me/ });
    fireEvent.click(prompt);
    expect(screen.getByPlaceholderText(/Ask Chief to run/)).toHaveValue(prompt.textContent);
  });

  it('normalizes legacy chef messages inside the private user-scoped history key', async () => {
    localStorage.setItem('tj-chats-v2:u1', JSON.stringify([
      { id: 'legacy', title: 'Legacy', messages: [{ who: 'chef', text: 'old private reply' }] },
    ]));
    withClient(<Dashboard nav={nav} userId="u1" />);
    const message = await screen.findByText('old private reply');
    expect(message.parentElement?.querySelector('.who')).toHaveTextContent('Chief');
    expect(screen.queryByText('Chef')).not.toBeInTheDocument();
  });
});
