// Thin client for the `/api/beads` surface shipped by gt-web (hq-fe-api-w.3 + .4).
// `listBeads` returns one column at a time; `transitionBead` POSTs the target status
// and lets the server's operator matrix reject illegal moves (caller surfaces the
// AppError text 1:1).

import type { Bead } from '$lib/types/bead';
import type { BeadStatus } from '$lib/kanban';

export async function listBeads(
  status: BeadStatus,
  fetchFn: typeof fetch = fetch
): Promise<Bead[]> {
  const url = `/api/beads?status=${encodeURIComponent(status)}`;
  const res = await fetchFn(url, { headers: { accept: 'application/json' } });
  if (!res.ok) {
    throw new Error(`GET ${url}: ${res.status} ${res.statusText}`);
  }
  return (await res.json()) as Bead[];
}

export async function transitionBead(
  id: string,
  to: BeadStatus,
  fetchFn: typeof fetch = fetch
): Promise<Bead> {
  const url = `/api/beads/${encodeURIComponent(id)}/transition`;
  const res = await fetchFn(url, {
    method: 'POST',
    headers: { accept: 'application/json', 'content-type': 'application/json' },
    body: JSON.stringify({ to })
  });
  if (!res.ok) {
    const body = await res.text().catch(() => '');
    throw new Error(`POST ${url}: ${res.status} ${res.statusText} ${body}`.trim());
  }
  return (await res.json()) as Bead;
}
