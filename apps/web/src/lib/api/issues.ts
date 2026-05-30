// Thin client for `GET /api/issues?status=open,working` (hq-fe-api-r.9).
// Uses the shared client (hq-fe-build.2).

import type { Issue } from '$lib/types/issue';
import { apiGet, type ApiRequestOpts } from './client';

export function fetchIssues(
  status: string = 'open,working',
  opts?: Omit<ApiRequestOpts, 'method' | 'body'>
): Promise<Issue[]> {
  return apiGet<Issue[]>(`/api/issues?status=${encodeURIComponent(status)}`, opts);
}
