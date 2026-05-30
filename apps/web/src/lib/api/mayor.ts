// Thin client for `GET /api/mayor/status` (hq-fe-api-r.7). Uses the shared
// client (hq-fe-build.2). Read-only snapshot, safe to poll.

import type { MayorStatus } from '$lib/types/mayor';
import { apiGet, type ApiRequestOpts } from './client';

export function fetchMayorStatus(
  opts?: Omit<ApiRequestOpts, 'method' | 'body'>
): Promise<MayorStatus> {
  return apiGet<MayorStatus>('/api/mayor/status', opts);
}
