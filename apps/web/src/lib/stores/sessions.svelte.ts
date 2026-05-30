// Sessions store · runes singleton (hq-fe-build.4).
//
// Source of truth for the live polecat/dog/mayor registry. Hydrated from
// `GET /api/sessions`; reconciles incrementally via `apply(EventRecord)`
// over the `agent.*` slice of the SSE bus. The projection is intentionally
// thin — every agent event is either an upsert (the new state lands as the
// `state` column) or a delete (`agent.session_end`), so the store does no
// reordering on top of insertion order; the UI sorts by the columns it
// cares about (Sessions table per hq-fe-view.4).

import type { EventRecord } from '$lib/types/event';
import type { Session } from '$lib/types/session';

class Sessions {
  rows = $state<Session[]>([]);

  hydrate(initial: Session[]): void {
    this.rows = [...initial];
  }

  /** Upsert/remove from one SSE frame. Ignores frames whose kind isn't `agent.*`. */
  apply(rec: EventRecord): void {
    if (!rec.type.startsWith('agent.')) return;
    const p = rec.payload as { session?: string; rig?: string; role?: string; crew?: string | null };
    const id = p?.session;
    if (!id) return;
    switch (rec.type) {
      case 'agent.session_end':
      case 'agent.killed':
        this.rows = this.rows.filter((r) => r.id !== id);
        return;
      case 'agent.spawned':
        this.upsert({
          id,
          rig: p.rig ?? '',
          state: 'spawned',
          role: p.role ?? 'polecat',
          crew: p.crew ?? null,
        });
        return;
      case 'agent.transition': {
        // Payload carries `to` as the new state on the lifecycle FSM.
        const to = (rec.payload as { to?: string })?.to;
        if (to) this.patch(id, { state: to });
        return;
      }
      case 'agent.heartbeat':
        // Heartbeats don't mutate the row; the store would gain a `last_seen` column once
        // the wire shape grows that field. Until then, swallow silently.
        return;
      default:
        // Unknown agent.* kind — ignore rather than mutate randomly.
        return;
    }
  }

  byId(id: string): Session | undefined {
    return this.rows.find((r) => r.id === id);
  }

  reset(): void {
    this.rows = [];
  }

  private upsert(row: Session): void {
    const i = this.rows.findIndex((r) => r.id === row.id);
    if (i === -1) this.rows = [...this.rows, row];
    else this.rows = this.rows.map((r, j) => (j === i ? { ...r, ...row } : r));
  }

  private patch(id: string, fields: Partial<Session>): void {
    this.rows = this.rows.map((r) => (r.id === id ? { ...r, ...fields } : r));
  }
}

export const sessions = new Sessions();
