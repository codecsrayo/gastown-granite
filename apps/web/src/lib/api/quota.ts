// Thin client for `GET /api/quota/rotation?since=&limit=` (hq-fe-api-r.2). Composite
// snapshot for the rotation panel: live `Cooldown` accounts joined with the tail of
// `quota.rotated` records pulled from `events.jsonl`.

import type { QuotaRotation } from '$lib/types/quota';
import { apiGet, type ApiRequestOpts } from './client';

export interface FetchQuotaRotationOpts extends Omit<ApiRequestOpts, 'method' | 'body'> {
  /** RFC3339 timestamp; only `recent_rotations` strictly newer than this are returned. */
  since?: string;
  /** Tail cap on `recent_rotations` (server default 50, max 500). */
  limit?: number;
}

export function fetchQuotaRotation(
  opts: FetchQuotaRotationOpts = {}
): Promise<QuotaRotation> {
  const { since, limit, ...rest } = opts;
  const qs = new URLSearchParams();
  if (since) qs.set('since', since);
  if (limit !== undefined) qs.set('limit', String(limit));
  const suffix = qs.toString();
  const url = suffix ? `/api/quota/rotation?${suffix}` : '/api/quota/rotation';
  return apiGet<QuotaRotation>(url, rest);
}
