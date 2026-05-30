// `GET /api/convoys[?state=launched]` (hq-fe-api-r.3). Snapshot of the orchestrator's
// convoy board. Optional `state` filter is applied server-side so the dashboard never
// re-walks the full slice client-side just to render a single column. Built on the
// shared client (hq-fe-build.2) so bearer + idem-key + ApiError shape stay uniform.

import { apiGet, type ApiRequestOpts } from './client';
import type { Convoy } from '$lib/types/convoy';

export function fetchConvoys(
  state?: string,
  opts?: Omit<ApiRequestOpts, 'method' | 'body'>,
): Promise<Convoy[]> {
  const path = state ? `/api/convoys?state=${encodeURIComponent(state)}` : '/api/convoys';
  return apiGet<Convoy[]>(path, opts);
}
