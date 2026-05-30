// Thin clients for the quota read-side: snapshot of every account (hq-fe-api-r.1) +
// composite rotation panel (hq-fe-api-r.2). Both endpoints return empty arrays — never
// 404 — when the bus is unwired, so the sidebar renders a stable shell without conditional
// rendering on the wire shape.

import type { QuotaAccount, QuotaRotation } from '$lib/types/quota';
import { apiGet, type ApiRequestOpts } from './client';

export type FetchQuotaAccountsOpts = Omit<ApiRequestOpts, 'method' | 'body'>;

export function fetchQuotaAccounts(
  opts: FetchQuotaAccountsOpts = {}
): Promise<QuotaAccount[]> {
  return apiGet<QuotaAccount[]>('/api/quota/accounts', opts);
}

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
