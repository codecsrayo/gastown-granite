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

  constructor(capacity = DEFAULT_CAPACITY) {
    this.capacity = capacity;
  }

  /** Seed from a historical snapshot (`/api/feed?since=…` once hq-fe-api-r.5 lands).
   *  Trims to capacity so a generous server response can't OOM the page. */
  hydrate(initial: EventRecord[]): void {
    this.events = initial.slice(-this.capacity);
  }

  /** Append one live frame. Drops the oldest when at capacity. */
  push(rec: EventRecord): void {
    if (this.events.length >= this.capacity) {
      this.events = [...this.events.slice(this.events.length - this.capacity + 1), rec];
    } else {
      this.events = [...this.events, rec];
    }
  }

  reset(): void {
    this.events = [];
  }
}

export const activity = new Activity();
/** Test-only factory so the vitest suite can pin a small capacity. Not exported from
 *  `$lib` — consumers should always reach for the `activity` singleton. */
export function _createActivityStore(capacity: number): Activity {
  return new Activity(capacity);
}
