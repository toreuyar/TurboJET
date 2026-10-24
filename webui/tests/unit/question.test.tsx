import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { QuestionCard, parseQuestionPayload, parseQuestionRecord, StageChat } from '../../src/views/Runs';
import { api } from '../../src/api';
import type { StageLog } from '../../src/types';

vi.mock('../../src/api', () => ({
  api: {
    sendQuestionAnswer: vi.fn(async () => ({ status: 'accepted', question_id: 'q1', replayed: false })),
    sendInput: vi.fn(async () => ({ status: 'delivered', seq: 1 })),
    questions: vi.fn(async () => ({ questions: [] })),
    cancelInput: vi.fn(async () => ({ cancelled: true })),
    inputStatus: vi.fn(async () => ({ bridge_active: true, waiting: true, stage_messages: [], queued: [] })),
    stageLogs: vi.fn(async () => [] as StageLog[]),
    stageLogStream: vi.fn(() => () => {}),
  },
  subscribeEvents: vi.fn(() => () => {}),
  getToken: vi.fn(() => 'tok'),
  setToken: vi.fn(),
}));

class ROStub { observe() {} unobserve() {} disconnect() {} }
if (!('ResizeObserver' in globalThis)) (globalThis as Record<string, unknown>).ResizeObserver = ROStub;

function withClient(el: React.ReactElement) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={qc}>{el}</QueryClientProvider>);
}

const SINGLE = { question: 'Which database?', options: [{ id: 'postgres', label: 'PostgreSQL' }, { id: 'sqlite', label: 'SQLite' }], multi_select: false, free_form: true };
const MULTI = { question: 'Which features?', options: [{ id: 'a', label: 'Alpha' }, { id: 'b', label: 'Beta' }, { id: 'c', label: 'Gamma' }], multi_select: true, free_form: false };

function row(id: number, stageId: string, kind: string, content: string): StageLog {
  return { id, run_id: 'run1', stage_id: stageId, stage_name: 'start', flow_name: 'flow-a', kind, content, created_at: '2026-09-10T10:00:00Z' };
}

function questionPayload(request: object, questionId = 'q1', stageId = 's1', attempt = 0) {
  return JSON.stringify({ ...request, question_id: questionId, stage_id: stageId, attempt, status: 'open' });
}

function card(question = SINGLE, questionId = 'q1') {
  return <QuestionCard runId="run1" stageId="s1" attempt={0} questionId={questionId} stageName="start" question={question} />;
}

describe('parseQuestionPayload', () => {
  it('parses a valid structured question row', () => {
    expect(parseQuestionPayload(JSON.stringify(SINGLE))).toEqual(SINGLE);
  });

  it('accepts free-form-only questions (empty options)', () => {
    expect(parseQuestionPayload('{"question":"Describe it","options":[],"multi_select":false,"free_form":true}')).toEqual({
      question: 'Describe it', options: [], multi_select: false, free_form: true,
    });
  });

  it('rejects plain text, JSON scalars, and degenerate asks (legacy payloads)', () => {
    expect(parseQuestionPayload('Which colour do you prefer?')).toBeNull();
    expect(parseQuestionPayload('"teal"')).toBeNull();
    expect(parseQuestionPayload('{"question":"…","options":[],"multi_select":false,"free_form":false}')).toBeNull();
    expect(parseQuestionPayload('{"options":[],"free_form":true}')).toBeNull();
    expect(parseQuestionPayload('{"question":"…","options":[{"id":1,"label":"x"}],"free_form":true}')).toBeNull();
  });
});

describe('QuestionCard', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    localStorage.clear();
    Object.assign(navigator, { clipboard: { writeText: vi.fn(async () => undefined) } });
  });

  it('single-select renders radios and submits the exact OQ-2 JSON answer', async () => {
    withClient(card(SINGLE));
    const radios = screen.getAllByRole('radio');
    expect(radios).toHaveLength(2);
    fireEvent.click(screen.getByLabelText('PostgreSQL'));
    expect(screen.getByLabelText('PostgreSQL')).toBeChecked();
    expect(screen.getByLabelText('SQLite')).not.toBeChecked();
    fireEvent.change(screen.getByLabelText('Free-form answer'), { target: { value: 'prefer hosted' } });
    fireEvent.click(screen.getByRole('button', { name: 'Submit answer' }));
    await waitFor(() => expect(api.sendQuestionAnswer).toHaveBeenCalledTimes(1));
    expect(api.sendQuestionAnswer).toHaveBeenCalledWith('run1', { selected: ['postgres'], text: 'prefer hosted' }, expect.objectContaining({ stageId: 's1', attempt: 0, questionId: 'q1', idempotencyKey: expect.any(String) }));
  });

  it('single-select replaces the choice; submit serializes without the text key when empty', async () => {
    withClient(card(SINGLE));
    fireEvent.click(screen.getByLabelText('PostgreSQL'));
    fireEvent.click(screen.getByLabelText('SQLite'));
    fireEvent.click(screen.getByRole('button', { name: 'Submit answer' }));
    await waitFor(() => expect(api.sendQuestionAnswer).toHaveBeenCalledTimes(1));
    expect(api.sendQuestionAnswer).toHaveBeenCalledWith('run1', { selected: ['sqlite'], text: undefined }, expect.objectContaining({ stageId: 's1', attempt: 0, questionId: 'q1' }));
    const wire = JSON.stringify(vi.mocked(api.sendQuestionAnswer).mock.calls[0][1]);
    expect(wire).toBe('{"selected":["sqlite"]}'); // text dropped — server Option<String> defaults
  });

  it('multi-select renders checkboxes and posts every selected id', async () => {
    withClient(card(MULTI));
    const boxes = screen.getAllByRole('checkbox');
    expect(boxes).toHaveLength(3);
    fireEvent.click(screen.getByLabelText('Alpha'));
    fireEvent.click(screen.getByLabelText('Gamma'));
    fireEvent.click(screen.getByLabelText('Alpha')); // toggle off again
    fireEvent.click(screen.getByRole('button', { name: 'Submit answer' }));
    await waitFor(() => expect(api.sendQuestionAnswer).toHaveBeenCalledTimes(1));
    expect(api.sendQuestionAnswer).toHaveBeenCalledWith('run1', { selected: ['c'], text: undefined }, expect.objectContaining({ stageId: 's1', attempt: 0, questionId: 'q1' }));
  });

  it('disables submit until an option or free text is provided', () => {
    withClient(card(MULTI));
    expect(screen.getByRole('button', { name: 'Submit answer' })).toBeDisabled();
    fireEvent.click(screen.getByLabelText('Beta'));
    expect(screen.getByRole('button', { name: 'Submit answer' })).toBeEnabled();
  });

  it('free-form-only question needs text and sends the typed answer', async () => {
    withClient(card({ question: 'Describe it', options: [], multi_select: false, free_form: true }));
    expect(screen.queryAllByRole('radio').length + screen.queryAllByRole('checkbox').length).toBe(0);
    const field = screen.getByLabelText('Free-form answer');
    expect(screen.getByRole('button', { name: 'Submit answer' })).toBeDisabled();
    fireEvent.change(field, { target: { value: 'It ingests CSVs' } });
    fireEvent.click(screen.getByRole('button', { name: 'Submit answer' }));
    await waitFor(() => expect(api.sendQuestionAnswer).toHaveBeenCalledWith('run1', { selected: [], text: 'It ingests CSVs' }, expect.objectContaining({ stageId: 's1', attempt: 0, questionId: 'q1' })));
  });

  it('shows the sent note and invalidates the input status after submit', async () => {
    withClient(card(SINGLE));
    fireEvent.click(screen.getByLabelText('PostgreSQL'));
    fireEvent.click(screen.getByRole('button', { name: 'Submit answer' }));
    await screen.findByText(/continuing with your answer/);
    expect(screen.queryByRole('button', { name: 'Submit answer' })).not.toBeInTheDocument();
  });
});

describe('StageChat question rendering', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    localStorage.clear();
    Object.assign(navigator, { clipboard: { writeText: vi.fn(async () => undefined) } });
  });

  function chat(stageStatus: string) {
    return <StageChat runId="run1" stageId="s1" stageIteration={0} stageName="start" stageStatus={stageStatus} />;
  }

  it('renders the question card for a waiting stage with a structured question', async () => {
    vi.mocked(api.stageLogs).mockResolvedValue([
      row(1, 's1', 'agent_message', '**start asks:**\nWhich database?'),
      row(2, 's1', 'question', questionPayload(SINGLE)),
    ]);
    withClient(chat('waiting_for_input'));
    expect(await screen.findByText('Which database?')).toBeInTheDocument();
    expect(screen.getAllByRole('radio')).toHaveLength(2);
    expect(screen.getByRole('button', { name: 'Submit answer' })).toBeInTheDocument();
    // The plain input box is replaced by the structured card.
    expect(screen.queryByPlaceholderText(/Reply to start/)).not.toBeInTheDocument();
  });

  it('keeps the plain input box for plain-text questions (legacy payloads)', async () => {
    vi.mocked(api.stageLogs).mockResolvedValue([row(1, 's1', 'agent_message', 'Which colour do you prefer?')]);
    withClient(chat('waiting_for_input'));
    const box = await screen.findByPlaceholderText(/Reply to start/);
    expect(box).toBeEnabled();
    expect(screen.queryByRole('button', { name: 'Submit answer' })).not.toBeInTheDocument();
    fireEvent.change(box, { target: { value: 'plain reply' } });
    fireEvent.click(screen.getByRole('button', { name: 'Send' }));
    await waitFor(() => expect(api.sendInput).toHaveBeenCalledWith('run1', 'plain reply', expect.objectContaining({ stageId: 's1', attempt: 0, idempotencyKey: expect.any(String) })));
  });

  it('ignores question rows belonging to other stages', async () => {
    vi.mocked(api.stageLogs).mockResolvedValue([row(1, 'other', 'question', questionPayload(SINGLE, 'q-other', 'other'))]);
    withClient(chat('waiting_for_input'));
    expect(await screen.findByPlaceholderText(/Reply to start/)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Submit answer' })).not.toBeInTheDocument();
  });

  it('never renders the card once the stage is no longer waiting (read-only history)', async () => {
    vi.mocked(api.stageLogs).mockResolvedValue([row(1, 's1', 'question', questionPayload(SINGLE))]);
    withClient(chat('completed'));
    await waitFor(() => expect(api.stageLogs).toHaveBeenCalled());
    expect(screen.queryByText('Submit answer')).not.toBeInTheDocument();
  });
  it('parses durable question identity and keeps legacy rows as plain text', () => {
    const parsed = parseQuestionRecord(questionPayload(SINGLE));
    expect(parsed).toMatchObject({ questionId: 'q1', stageId: 's1', attempt: 0, status: 'open' });
    expect(parseQuestionPayload(JSON.stringify(SINGLE))).toEqual(SINGLE);
    expect(parseQuestionRecord(JSON.stringify(SINGLE))).toBeNull();
  });

  it('renders and exports only the selected stage, never another stage turn', async () => {
    vi.mocked(api.stageLogs).mockResolvedValue([
      row(1, 's1', 'agent_message', 'selected stage message'),
      row(2, 's2', 'agent_message', 'other stage secret'),
      row(3, 's2', 'user_input', 'other stage answer'),
    ]);
    withClient(chat('waiting_for_input'));
    await waitFor(() => expect(api.stageLogs).toHaveBeenCalled());
    await new Promise((resolve) => setTimeout(resolve, 50));
    fireEvent.click(screen.getByRole('button', { name: 'Export' }));
    await waitFor(() => expect(navigator.clipboard.writeText).toHaveBeenCalled());
    const exported = vi.mocked(navigator.clipboard.writeText).mock.calls[0][0];
    expect(exported).toContain('Stage: s1 · attempt 1');
    expect(exported).toContain('selected stage message');
    expect(exported).not.toContain('other stage secret');
  });

  it('a new question id remounts the card and clears the previous selection', async () => {
    vi.mocked(api.stageLogs).mockResolvedValue([
      row(1, 's1', 'question', questionPayload(SINGLE, 'q1')),
    ]);
    vi.mocked(api.questions).mockResolvedValue({ questions: [] });
    withClient(chat('waiting_for_input'));
    fireEvent.click(await screen.findByLabelText('PostgreSQL'));
    expect(screen.getByLabelText('PostgreSQL')).toBeChecked();
    // The stream delivers a new open question with identical text but a new id.
    const callback = vi.mocked(api.stageLogStream).mock.calls[0][1];
    callback([row(2, 's1', 'question', questionPayload(SINGLE, 'q2'))]);
    await waitFor(() => expect(screen.getByLabelText('PostgreSQL')).not.toBeChecked());
    expect(screen.getByLabelText('SQLite')).not.toBeChecked();
  });

});
