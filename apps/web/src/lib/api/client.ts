// Central fetch wrapper for `/api/*`. Three responsibilities:
//   1. Inject `Authorization: Bearer <token>` from `lib/bearer.ts`, except
//      when the dev sentinel `dev` is set — the +layout.ts guard accepts the
//      sentinel so the SPA renders without a real JWT, and the backend's
//      `auth=open` posture skips the check entirely (apps/api).
//   2. Generate a per-request `Idempotency-Key` for non-GET methods so
//      gt-web's idempotency middleware (hq-fe-api-w.2) can dedupe retries.
//   3. Convert non-2xx into a typed `ApiError` carrying status + raw body,
//      and call the registered 401 hook (typically a `goto('/login')`).
//
// Tests live in `client.test.ts`; consumers (`api/{issues,sessions,
// worktrees,beads}.ts`) keep their thin domain wrappers and just call
// `apiGet`/`apiSend`.

import { readBearer } from '$lib/bearer';

const DEV_SENTINEL = 'dev';

export type ApiMethod = 'GET' | 'POST' | 'PATCH' | 'PUT' | 'DELETE';

export class ApiError extends Error {
  constructor(
    public status: number,
    public method: ApiMethod,
    public path: string,
    public body: string
  ) {
    super(`${method} ${path}: ${status} ${body}`.trim());
    this.name = 'ApiError';
  }
}

export interface ApiRequestOpts {
  method?: ApiMethod;
  body?: unknown;
  idempotencyKey?: string;
  fetchFn?: typeof fetch;
  // Skip the 401 hook for this request (e.g. polling endpoints where the
  // caller wants to surface the error inline instead of yanking the page).
  skip401Hook?: boolean;
}

let on401Handler: (() => void) | null = null;

/** Register a global hook fired on the first 401. Pass `null` to clear. */
export function setOn401(handler: (() => void) | null): void {
  on401Handler = handler;
}

function makeIdemKey(): string {
  if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
    return crypto.randomUUID();
  }
  return `idem-${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

export async function apiRequest(path: string, opts: ApiRequestOpts = {}): Promise<Response> {
  const method = opts.method ?? 'GET';
  const fetchFn = opts.fetchFn ?? fetch;
  const headers: Record<string, string> = { accept: 'application/json' };

  const bearer = readBearer();
  if (bearer && bearer !== DEV_SENTINEL) {
    headers['authorization'] = `Bearer ${bearer}`;
  }

  let body: BodyInit | undefined;
  if (opts.body !== undefined) {
    headers['content-type'] = 'application/json';
    body = JSON.stringify(opts.body);
  }

  if (method !== 'GET') {
    headers['idempotency-key'] = opts.idempotencyKey ?? makeIdemKey();
  }

  const res = await fetchFn(path, { method, headers, body });

  if (res.status === 401 && !opts.skip401Hook && on401Handler) {
    on401Handler();
  }

  if (!res.ok) {
    const text = await res.text().catch(() => '');
    throw new ApiError(res.status, method, path, text);
  }
  return res;
}

export async function apiGet<T>(
  path: string,
  opts?: Omit<ApiRequestOpts, 'method' | 'body'>
): Promise<T> {
  const res = await apiRequest(path, { ...(opts ?? {}), method: 'GET' });
  return (await res.json()) as T;
}

export async function apiSend<T>(
  method: Exclude<ApiMethod, 'GET'>,
  path: string,
  body?: unknown,
  opts?: Omit<ApiRequestOpts, 'method' | 'body'>
): Promise<T> {
  const res = await apiRequest(path, { ...(opts ?? {}), method, body });
  return (await res.json()) as T;
}
