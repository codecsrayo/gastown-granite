import { describe, expect, it, vi } from 'vitest';
import { failConvoyMember, fetchConvoys } from './convoys';

function jsonResponse(data: unknown, init: ResponseInit = {}): Response {
  return new Response(JSON.stringify(data), {
    status: 200,
    headers: { 'content-type': 'application/json' },
    ...init,
  });
}

describe('convoys api', () => {
  it('fetchConvoys hits /api/convoys with no query when state is unset', async () => {
    const calls: string[] = [];
    const fetchFn = vi.fn(async (input: RequestInfo | URL) => {
      calls.push(String(input));
      return jsonResponse([]);
    }) as unknown as typeof fetch;
    await fetchConvoys(undefined, { fetchFn });
    expect(calls[0]).toBe('/api/convoys');
  });

  it('fetchConvoys passes state through ?state= with encoding', async () => {
    const calls: string[] = [];
    const fetchFn = vi.fn(async (input: RequestInfo | URL) => {
      calls.push(String(input));
      return jsonResponse([]);
    }) as unknown as typeof fetch;
    await fetchConvoys('launched', { fetchFn });
    expect(calls[0]).toBe('/api/convoys?state=launched');
  });

  it('failConvoyMember POSTs to nested path with JSON reason body', async () => {
    let captured: { url: string; init?: RequestInit } | null = null;
    const fetchFn = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      captured = { url: String(input), init };
      return jsonResponse({ failed: true, convoy: 'cv-1', member: 'hq-a.1' });
    }) as unknown as typeof fetch;
    const out = await failConvoyMember('cv-1', 'hq-a.1', 'stuck on review', { fetchFn });
    expect(captured?.url).toBe('/api/convoys/cv-1/members/hq-a.1/fail');
    expect(captured?.init?.method).toBe('POST');
    const headers = captured?.init?.headers as Record<string, string>;
    expect(headers['content-type']).toBe('application/json');
    expect(headers['idempotency-key']).toBeTruthy();
    expect(JSON.parse(String(captured?.init?.body))).toEqual({ reason: 'stuck on review' });
    expect(out).toEqual({ failed: true, convoy: 'cv-1', member: 'hq-a.1' });
  });

  it('failConvoyMember encodes path segments so slashes/special chars are safe', async () => {
    const calls: string[] = [];
    const fetchFn = vi.fn(async (input: RequestInfo | URL) => {
      calls.push(String(input));
      return jsonResponse({ failed: true, convoy: 'cv/1', member: 'hq a.1' });
    }) as unknown as typeof fetch;
    await failConvoyMember('cv/1', 'hq a.1', 'why', { fetchFn });
    expect(calls[0]).toBe('/api/convoys/cv%2F1/members/hq%20a.1/fail');
  });
});
