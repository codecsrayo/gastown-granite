// Thin client for `GET /api/whoami` (hq-fe-rbac.4). Uses the shared client
// (hq-fe-build.2). Called once on layout mount to hydrate the auth store.

import type { Whoami } from '$lib/types/whoami';
import { apiGet, type ApiRequestOpts } from './client';

export function fetchWhoami(
  opts?: Omit<ApiRequestOpts, 'method' | 'body'>
): Promise<Whoami> {
  return apiGet<Whoami>('/api/whoami', opts);
}
