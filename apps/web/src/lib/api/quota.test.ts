import { describe, expect, it, vi } from 'vitest';
import { fetchQuotaAccounts, fetchQuotaRotation } from './quota';

function jsonResponse(data: unknown): Response {
  return new Response(JSON.stringify(data), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
}

describe('quota api', () => {
  it('fetchQuotaAccounts hits /api/quota/accounts as GET', async () => {
    const calls: { url: string; method: string | undefined }[] = [];
    const fetchFn = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      calls.push({ url: String(input), method: init?.method });
      return jsonResponse([]);
    }) as unknown as typeof fetch;
    const out = await fetchQuotaAccounts({ fetchFn });
    expect(calls[0]?.url).toBe('/api/quota/accounts');
    expect(calls[0]?.method ?? 'GET').toBe('GET');
    expect(out).toEqual([]);
  });

  it('fetchQuotaAccounts returns the parsed array shape', async () => {
    const rows = [
      {
        id: 'brayan',
        state: 'active',
        tokens_used: 100,
        tokens_cap: 1000,
        reset_at: 1_780_000_000,
        sessions: [],
      },
    ];
    const fetchFn = vi.fn(async () => jsonResponse(rows)) as unknown as typeof fetch;
    const out = await fetchQuotaAccounts({ fetchFn });
    expect(out).toHaveLength(1);
    expect(out[0]?.id).toBe('brayan');
    expect(out[0]?.state).toBe('active');
  });

  it('fetchQuotaRotation passes since + limit via query string', async () => {
    const calls: string[] = [];
    const fetchFn = vi.fn(async (input: RequestInfo | URL) => {
      calls.push(String(input));
      return jsonResponse({ waiting_unlock: [], recent_rotations: [] });
    }) as unknown as typeof fetch;
    await fetchQuotaRotation({ since: '2026-05-30T10:00:00Z', limit: 16, fetchFn });
    const url = new URL(calls[0]!, 'http://t');
    expect(url.pathname).toBe('/api/quota/rotation');
    expect(url.searchParams.get('since')).toBe('2026-05-30T10:00:00Z');
    expect(url.searchParams.get('limit')).toBe('16');
  });

  it('fetchQuotaRotation hits the bare path when no opts are passed', async () => {
    const calls: string[] = [];
    const fetchFn = vi.fn(async (input: RequestInfo | URL) => {
      calls.push(String(input));
      return jsonResponse({ waiting_unlock: [], recent_rotations: [] });
    }) as unknown as typeof fetch;
    await fetchQuotaRotation({ fetchFn });
    expect(calls[0]).toBe('/api/quota/rotation');
  });
});
