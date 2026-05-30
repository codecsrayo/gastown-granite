import { describe, expect, it } from 'vitest';
import type { EventRecord } from '$lib/types/event';
import { _createActivityStore } from './activity.svelte';

function ev(i: number): EventRecord {
  return {
    event_id: `e${i}`,
    correlation_id: 'c',
    causation_id: null,
    ts: '2026-05-30T00:00:00Z',
    type: 'agent.spawned',
    payload: {},
  };
}

describe('activity store', () => {
  it('push grows up to capacity', () => {
    const s = _createActivityStore(3);
    s.push(ev(1));
    s.push(ev(2));
    expect(s.events.map((e) => e.event_id)).toEqual(['e1', 'e2']);
  });

  it('push evicts oldest once at capacity', () => {
    const s = _createActivityStore(3);
    s.push(ev(1));
    s.push(ev(2));
    s.push(ev(3));
    s.push(ev(4));
    s.push(ev(5));
    expect(s.events.map((e) => e.event_id)).toEqual(['e3', 'e4', 'e5']);
  });

  it('hydrate trims overlong snapshots to capacity', () => {
    const s = _createActivityStore(3);
    s.hydrate([ev(1), ev(2), ev(3), ev(4), ev(5)]);
    expect(s.events.map((e) => e.event_id)).toEqual(['e3', 'e4', 'e5']);
  });

  it('reset empties the buffer', () => {
    const s = _createActivityStore(3);
    s.push(ev(1));
    s.reset();
    expect(s.events).toEqual([]);
  });
});
