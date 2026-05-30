// Thin client for `GET /api/merges` (hq-fe-api-r.4). Uses the shared client
// (hq-fe-build.2). Read-only snapshot of the merge slot board.

import type { MergeSlot } from '$lib/types/merge';
import { apiGet, type ApiRequestOpts } from './client';

export function fetchMerges(
  opts?: Omit<ApiRequestOpts, 'method' | 'body'>
): Promise<MergeSlot[]> {
  return apiGet<MergeSlot[]>('/api/merges', opts);
}
