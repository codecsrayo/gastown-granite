import { describe, expect, it, beforeEach, afterEach, vi } from 'vitest';
import { apiGet, apiSend, apiRequest, setOn401, ApiError } from './client';
import { writeBearer, clearBearer } from '$lib/bearer';

function jsonResponse(data: unknown, init: ResponseInit = {}): Response {
  return new Response(JSON.stringify(data), {
    status: 200,
    headers: { 'content-type': 'application/json' },
    ...init
  });
}

function captureFetch(impl: (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>) {
  const calls: Array<{ url: string; init?: RequestInit }> = [];
  const fetchFn = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    calls.push({ url: String(input), init });
    return impl(input, init);
  }) as unknown as typeof fetch;
  return { fetchFn, calls };
}

describe('api client', () => {
  beforeEach(() => {
    localStorage.clear();
    setOn401(null);
  });
  afterEach(() => {
    clearBearer();
    setOn401(null);
  });

  it('apiGet sends no Authorization header without a bearer', async () => {
    const { fetchFn, calls } = captureFetch(async () => jsonResponse({ ok: true }));
    await apiGet<{ ok: boolean }>('/api/ping', { fetchFn });
    const headers = calls[0].init?.headers as Record<string, string>;
    expect(headers.authorization).toBeUndefined();
    expect(headers.accept).toBe('application/json');
  });

  it('apiGet attaches Bearer when a real token is present', async () => {
    writeBearer('eyJ.tok.en');
    const { fetchFn, calls } = captureFetch(async () => jsonResponse({}));
    await apiGet('/api/ping', { fetchFn });
    const headers = calls[0].init?.headers as Record<string, string>;
    expect(headers.authorization).toBe('Bearer eyJ.tok.en');
  });

  it('skips Authorization when the dev sentinel is set', async () => {
    writeBearer('dev');
    const { fetchFn, calls } = captureFetch(async () => jsonResponse({}));
    await apiGet('/api/ping', { fetchFn });
    const headers = calls[0].init?.headers as Record<string, string>;
    expect(headers.authorization).toBeUndefined();
  });

  it('apiSend auto-generates an Idempotency-Key for POST', async () => {
    const { fetchFn, calls } = captureFetch(async () => jsonResponse({}));
    await apiSend('POST', '/api/things', { hello: 'world' }, { fetchFn });
    const headers = calls[0].init?.headers as Record<string, string>;
    expect(headers['idempotency-key']).toBeTypeOf('string');
    expect(headers['idempotency-key'].length).toBeGreaterThan(0);
    expect(headers['content-type']).toBe('application/json');
    expect(calls[0].init?.body).toBe(JSON.stringify({ hello: 'world' }));
  });

  it('apiSend honors an explicit idempotencyKey', async () => {
    const { fetchFn, calls } = captureFetch(async () => jsonResponse({}));
    await apiSend('POST', '/api/things', { x: 1 }, { fetchFn, idempotencyKey: 'pinned-123' });
    const headers = calls[0].init?.headers as Record<string, string>;
    expect(headers['idempotency-key']).toBe('pinned-123');
  });

  it('GET never carries Idempotency-Key', async () => {
    const { fetchFn, calls } = captureFetch(async () => jsonResponse({}));
    await apiGet('/api/ping', { fetchFn });
    const headers = calls[0].init?.headers as Record<string, string>;
    expect(headers['idempotency-key']).toBeUndefined();
  });

  it('throws ApiError with status + body on non-2xx', async () => {
    const { fetchFn } = captureFetch(async () =>
      new Response('boom', { status: 422, statusText: 'Unprocessable' })
    );
    await expect(apiGet('/api/oops', { fetchFn })).rejects.toMatchObject({
      name: 'ApiError',
      status: 422,
      method: 'GET',
      path: '/api/oops',
      body: 'boom'
    });
  });

  it('fires the 401 hook exactly once and still throws', async () => {
    const { fetchFn } = captureFetch(async () => new Response('nope', { status: 401 }));
    const hook = vi.fn();
    setOn401(hook);
    await expect(apiGet('/api/protected', { fetchFn })).rejects.toBeInstanceOf(ApiError);
    expect(hook).toHaveBeenCalledTimes(1);
  });

  it('respects skip401Hook for polling endpoints', async () => {
    const { fetchFn } = captureFetch(async () => new Response('', { status: 401 }));
    const hook = vi.fn();
    setOn401(hook);
    await expect(apiGet('/api/poll', { fetchFn, skip401Hook: true })).rejects.toBeInstanceOf(
      ApiError
    );
    expect(hook).not.toHaveBeenCalled();
  });

  it('apiRequest returns the raw Response so callers can stream', async () => {
    const { fetchFn } = captureFetch(async () =>
      new Response('plain text', {
        status: 200,
        headers: { 'content-type': 'text/plain' }
      })
    );
    const res = await apiRequest('/api/raw', { fetchFn });
    expect(await res.text()).toBe('plain text');
  });
});
