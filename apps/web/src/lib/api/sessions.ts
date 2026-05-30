// Thin client for `GET /api/sessions[?role=<role>]` (hq-fe-view.4). The endpoint snapshots
// the live polecat/dog/mayor registry; the dashboard polls it (no SSE channel for sessions
// yet — agent.* events flow through `/api/stream` but are noisy + need projection that the
// FE doesn't have plumbed yet).

import type { Session } from '$lib/types/session';

export async function fetchSessions(
  role?: string,
  fetchFn: typeof fetch = fetch
): Promise<Session[]> {
  const url = role ? `/api/sessions?role=${encodeURIComponent(role)}` : '/api/sessions';
  const res = await fetchFn(url, { headers: { accept: 'application/json' } });
  if (!res.ok) {
    throw new Error(`GET ${url}: ${res.status} ${res.statusText}`);
  }
  return (await res.json()) as Session[];
}
