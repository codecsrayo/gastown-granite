// Thin client for the `/api/beads` surface shipped by gt-web (hq-fe-api-w.3 + .4).
// `listBeads` returns one column at a time; `transitionBead` POSTs the target status
// and lets the server's operator matrix reject illegal moves (caller surfaces the
// ApiError text 1:1). Uses the shared client (hq-fe-build.2).

import type { Bead } from '$lib/types/bead';
import type { BeadStatus } from '$lib/kanban';
import { apiGet, apiSend, type ApiRequestOpts } from './client';

export function listBeads(
  status: BeadStatus,
  opts?: Omit<ApiRequestOpts, 'method' | 'body'>
): Promise<Bead[]> {
  return apiGet<Bead[]>(`/api/beads?status=${encodeURIComponent(status)}`, opts);
}

export function transitionBead(
  id: string,
  to: BeadStatus,
  opts?: Omit<ApiRequestOpts, 'method' | 'body'>
): Promise<Bead> {
  return apiSend<Bead>(
    'POST',
    `/api/beads/${encodeURIComponent(id)}/transition`,
    { to },
    opts
  );
}
