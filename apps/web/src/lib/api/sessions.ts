// Thin client for `GET /api/sessions[?role=<role>][&rig=<rig>]` (hq-fe-view.4
// + hq-fe-api-r.6). Uses the shared client (hq-fe-build.2) so bearer +
// idem-key + 401 hook stay consistent across every domain wrapper.

import type { Session } from '$lib/types/session';
import { apiGet, apiSend, type ApiRequestOpts } from './client';

export interface SessionsFilter {
  role?: string;
  rig?: string;
}

export function fetchSessions(
  filter: SessionsFilter | string = {},
  opts?: Omit<ApiRequestOpts, 'method' | 'body'>
): Promise<Session[]> {
  // Back-compat: callers that still pass a bare role string keep working.
  const f: SessionsFilter = typeof filter === 'string' ? { role: filter } : filter;
  const params = new URLSearchParams();
  if (f.role) params.set('role', f.role);
  if (f.rig) params.set('rig', f.rig);
  const qs = params.toString();
  const url = qs ? `/api/sessions?${qs}` : '/api/sessions';
  return apiGet<Session[]>(url, opts);
}

/** `DELETE /api/sessions/:id` — polecat e-stop (hq-fe-api-w.6). */
export function killSession(
  id: string,
  opts?: Omit<ApiRequestOpts, 'method' | 'body'>
): Promise<unknown> {
  return apiSend<unknown>('DELETE', `/api/sessions/${encodeURIComponent(id)}`, undefined, opts);
}
