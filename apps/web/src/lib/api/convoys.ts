// `GET /api/convoys[?state=launched]` (hq-fe-api-r.3) + `POST /api/convoys/:c/members/:m/fail`
// (hq-fe-api-w.9). Snapshot + per-member halt. Built on the shared client (hq-fe-build.2)
// so bearer + idem-key + ApiError shape stay uniform.

import { apiGet, apiSend, type ApiRequestOpts } from './client';
import type { Convoy } from '$lib/types/convoy';

export function fetchConvoys(
  state?: string,
  opts?: Omit<ApiRequestOpts, 'method' | 'body'>,
): Promise<Convoy[]> {
  const path = state ? `/api/convoys?state=${encodeURIComponent(state)}` : '/api/convoys';
  return apiGet<Convoy[]>(path, opts);
}

export interface FailMemberResponse {
  failed: boolean;
  convoy: string;
  member: string;
}

/** Halt a convoy at the failing member. The backend rejects empty `reason` with 400,
 *  so the caller must pass a non-empty operator-supplied "why". */
export function failConvoyMember(
  convoy: string,
  member: string,
  reason: string,
  opts?: Omit<ApiRequestOpts, 'method' | 'body'>,
): Promise<FailMemberResponse> {
  const path = `/api/convoys/${encodeURIComponent(convoy)}/members/${encodeURIComponent(member)}/fail`;
  return apiSend<FailMemberResponse>('POST', path, { reason }, opts);
}
