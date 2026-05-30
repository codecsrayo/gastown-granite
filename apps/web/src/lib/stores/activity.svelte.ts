// Activity store · runes singleton (hq-fe-build.4).
//
// Ring buffer of the last ~500 SSE frames. Powers the Activity tab
// (hq-fe-view.3, canon hero) + any view that wants a recent-event peek.
// Capacity is intentionally a constructor knob so the test suite can
// stress eviction with a tiny buffer instead of pumping 500 records.
//
// Filters (category / rig / kind) live in the view layer as `$derived`
// chains over `events`; the store stays projection-agnostic.

import type { EventRecord } from '$lib/types/event';

const DEFAULT_CAPACITY = 500;

class Activity {
  events = $state<EventRecord[]>([]);
  capacity: number;
  /** Mirror of `events`' `event_id`s so `push` can dedup in O(1). Needed because the
   *  `/api/feed` hydrate (hq-fe-api-r.5) overlaps with the live SSE stream: any frame
   *  emitted between snapshot read and subscribe registration lands in both. The keyed
   *  `{#each ... (e.event_id)}` in the view would warn on duplicate keys, so we drop
   *  them at the source. */
  #seen = new Set<string>();

  constructor(capacity = DEFAULT_CAPACITY) {
    this.capacity = capacity;
  }

  /** Seed from a historical snapshot (`/api/feed?since=…`, hq-fe-api-r.5).
   *  Trims to capacity so a generous server response can't OOM the page. */
  hydrate(initial: EventRecord[]): void {
    const slice = initial.slice(-this.capacity);
    this.events = slice;
    this.#seen = new Set(slice.map((e) => e.event_id));
  }

  /** Append one live frame. Drops the oldest when at capacity. Idempotent on
   *  `event_id` so the snapshot/SSE overlap window doesn't surface duplicates. */
  push(rec: EventRecord): void {
    if (this.#seen.has(rec.event_id)) return;
    if (this.events.length >= this.capacity) {
      const dropped = this.events[0];
      this.events = [...this.events.slice(this.events.length - this.capacity + 1), rec];
      if (dropped) this.#seen.delete(dropped.event_id);
    } else {
      this.events = [...this.events, rec];
    }
    this.#seen.add(rec.event_id);
  }

  reset(): void {
    this.events = [];
    this.#seen = new Set();
  }
}

export const activity = new Activity();
/** Test-only factory so the vitest suite can pin a small capacity. Not exported from
 *  `$lib` — consumers should always reach for the `activity` singleton. */
export function _createActivityStore(capacity: number): Activity {
  return new Activity(capacity);
}
