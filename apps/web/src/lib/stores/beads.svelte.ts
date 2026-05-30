// Beads store · runes singleton (hq-fe-build.4).
//
// Holds the slice of `hq.issues` the dashboard renders (Work kanban + the
// `/worktrees` cross-link). Hydrated from `GET /api/issues?status=...`;
// kept fresh by polling for now — gt-root doesn't broadcast hq.issues
// mutations over `/api/stream` yet, so there's no `apply(EventRecord)`
// path here. When that projection lands, mirror the sessions store's SSE
// shape and add the kind filter (`issues.*`).
//
// Index by id is kept lazy via a `byId()` getter rather than a Map field
// so derived `$derived` views can read it without invalidating on every
// mutation of an unrelated row.

import type { Issue } from '$lib/types/issue';

class Beads {
  rows = $state<Issue[]>([]);

  hydrate(initial: Issue[]): void {
    this.rows = [...initial];
  }

  byId(id: string): Issue | undefined {
    return this.rows.find((r) => r.id === id);
  }

  /** Optimistic patch — used by Work kanban drop handlers before the server confirms.
   *  When the server returns the canonical row, call `replace()` to reconcile. */
  patch(id: string, fields: Partial<Issue>): void {
    this.rows = this.rows.map((r) => (r.id === id ? { ...r, ...fields } : r));
  }

  /** Server-confirmed replace. Falls back to append when the row isn't in the slice yet
   *  (e.g. the user is on a status column the bead just transitioned into). */
  replace(row: Issue): void {
    const i = this.rows.findIndex((r) => r.id === row.id);
    if (i === -1) this.rows = [...this.rows, row];
    else this.rows = this.rows.map((r, j) => (j === i ? row : r));
  }

  remove(id: string): void {
    this.rows = this.rows.filter((r) => r.id !== id);
  }

  reset(): void {
    this.rows = [];
  }
}

export const beads = new Beads();
