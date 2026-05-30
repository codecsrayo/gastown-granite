// Thin client for `GET /api/sessions[?role=<role>]` (hq-fe-view.4). Uses the
// shared client (hq-fe-build.2) so bearer + idem-key + 401 hook stay
// consistent across every domain wrapper.

import type { Session } from '$lib/types/session';
import { apiGet, apiSend, type ApiRequestOpts } from './client';

export function fetchSessions(
  role?: string,
  opts?: Omit<ApiRequestOpts, 'method' | 'body'>
): Promise<Session[]> {
  const url = role ? `/api/sessions?role=${encodeURIComponent(role)}` : '/api/sessions';
  return apiGet<Session[]>(url, opts);
}

/** `DELETE /api/sessions/:id` — polecat e-stop (hq-fe-api-w.6). */
export function killSession(
  id: string,
  opts?: Omit<ApiRequestOpts, 'method' | 'body'>
): Promise<unknown> {
  return apiSend<unknown>('DELETE', `/api/sessions/${encodeURIComponent(id)}`, undefined, opts);
}
