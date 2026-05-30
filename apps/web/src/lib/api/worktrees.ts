// Thin client for `GET /api/worktrees` (hq-fe-api-r.8). Uses the shared
// client (hq-fe-build.2).

import type { Worktree } from '$lib/types/worktree';
import { apiGet, type ApiRequestOpts } from './client';

export function fetchWorktrees(
  opts?: Omit<ApiRequestOpts, 'method' | 'body'>
): Promise<Worktree[]> {
  return apiGet<Worktree[]>('/api/worktrees', opts);
}
