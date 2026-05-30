// Thin fetch client for `GET /api/worktrees` (hq-fe-api-r.8). The dev server proxies `/api`
// to gt-api on :8787 (vite.config.ts); production builds talk to the same origin. No bearer
// header here — IAM is added centrally once hq-fe-view.2 (/login + +layout.ts guard) lands.

import type { Worktree } from '$lib/types/worktree';

export async function fetchWorktrees(fetchFn: typeof fetch = fetch): Promise<Worktree[]> {
  const res = await fetchFn('/api/worktrees', { headers: { accept: 'application/json' } });
  if (!res.ok) {
    throw new Error(`GET /api/worktrees: ${res.status} ${res.statusText}`);
  }
  return (await res.json()) as Worktree[];
}
