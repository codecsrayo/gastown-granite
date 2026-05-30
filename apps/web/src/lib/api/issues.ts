// Thin client for `GET /api/issues?status=open,working` (hq-fe-api-r.9). The `/worktrees`
// panel reads this to enrich each claim-branch row with the bead title + assignee
// (hq-fe-view.15). `status` defaults to `open,working` because both are "live" from the
// dashboard's view — `open` is freshly minted (not yet claimed in hq.issues even when an
// agent is already pushing to a `claim/<bead-id>` branch), and `working` is the actively
// transitioned slice. Closed beads don't have a live worktree to label.

import type { Issue } from '$lib/types/issue';

export async function fetchIssues(
  status: string = 'open,working',
  fetchFn: typeof fetch = fetch
): Promise<Issue[]> {
  const url = `/api/issues?status=${encodeURIComponent(status)}`;
  const res = await fetchFn(url, { headers: { accept: 'application/json' } });
  if (!res.ok) {
    throw new Error(`GET ${url}: ${res.status} ${res.statusText}`);
  }
  return (await res.json()) as Issue[];
}
