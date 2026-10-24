import { afterEach, describe, expect, it, vi } from 'vitest';
import { api, getToken, setToken } from '../../src/api';

describe('api client', () => {
  afterEach(() => {
    setToken(null);
    vi.unstubAllGlobals();
  });

  it('stores and clears the token in localStorage', () => {
    setToken('abc');
    expect(getToken()).toBe('abc');
    expect(localStorage.getItem('tj-token')).toBe('abc');
    setToken(null);
    expect(getToken()).toBeNull();
    expect(localStorage.getItem('tj-token')).toBeNull();
  });

  it('sends the bearer header and parses error bodies with status', async () => {
    setToken('tok123');
    const fetchMock = vi.fn(async (_url: unknown, _opts?: unknown) => ({
      ok: false,
      status: 409,
      json: async () => ({ error: 'run is not waiting at a gate' }),
    })) as unknown as typeof fetch;
    vi.stubGlobal('fetch', fetchMock);
    await expect(api.approve('r1', {
      approved: true, gate_instance_id: 'gate_1', expected_state_version: 2, idempotency_key: 'idem-1',
    })).rejects.toMatchObject({
      message: 'run is not waiting at a gate',
      status: 409,
    });
    const call = (fetchMock as unknown as { mock: { calls: [string, RequestInit][] } }).mock.calls[0];
    expect((call[1]?.headers as Record<string, string>).Authorization).toBe('Bearer tok123');
  });

  it('approve passes reason null when omitted', async () => {
    const calls: [string, RequestInit | undefined][] = [];
    vi.stubGlobal('fetch', (async (url: string, opts?: RequestInit) => {
      calls.push([url, opts]);
      return { ok: true, status: 200, json: async () => ({ id: 'r1' }) };
    }) as unknown as typeof fetch);
    await api.approve('r1', { approved: false, reason: 'Too broad', gate_instance_id: 'gate_1', expected_state_version: 2, idempotency_key: 'idem-1' });
    await api.approve('r1', { approved: true, gate_instance_id: 'gate_1', expected_state_version: 2, idempotency_key: 'idem-2' });
    expect(JSON.parse(String(calls[0][1]!.body))).toEqual({
      approved: false, reason: 'Too broad', gate_instance_id: 'gate_1', expected_state_version: 2, idempotency_key: 'idem-1',
    });
    expect(JSON.parse(String(calls[1][1]!.body))).toEqual({
      approved: true, reason: null, gate_instance_id: 'gate_1', expected_state_version: 2, idempotency_key: 'idem-2',
    });
  });

  it('expires the local session on 401 but keeps it on project denial', async () => {
    const expired = vi.fn();
    window.addEventListener('tj-auth-expired', expired);
    setToken('session');
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: false, status: 403, json: async () => ({ error: 'access denied' }) }));
    await expect(api.runs()).rejects.toMatchObject({ status: 403 });
    expect(getToken()).toBe('session');
    expect(expired).not.toHaveBeenCalled();
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: false, status: 401, json: async () => ({ error: 'expired' }) }));
    await expect(api.runs()).rejects.toMatchObject({ status: 401 });
    expect(getToken()).toBeNull();
    expect(expired).toHaveBeenCalledOnce();
    window.removeEventListener('tj-auth-expired', expired);
  });
});
