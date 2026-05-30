// @vitest-environment node

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { EventRecord } from '$lib/types/event';
import { sse, type SseHandler, type SseStatus } from './sse';

/** Hand-rolled EventSource double — jsdom's implementation is jittery + we want to drive
 *  open/error/message events deterministically from the test. */
class FakeEventSource {
  static instances: FakeEventSource[] = [];
  readyState: number = 0;
  onopen: ((ev: Event) => void) | null = null;
  onerror: ((ev: Event) => void) | null = null;
  onmessage: ((ev: MessageEvent) => void) | null = null;
  closed = false;
  constructor(public url: string) {
    FakeEventSource.instances.push(this);
  }
  close(): void {
    this.closed = true;
  }
  emit(record: Partial<EventRecord>): void {
    this.onmessage?.({ data: JSON.stringify(record) } as MessageEvent);
  }
  emitRaw(data: string): void {
    this.onmessage?.({ data } as MessageEvent);
  }
  open(): void {
    this.onopen?.({} as Event);
  }
  error(): void {
    this.onerror?.({} as Event);
  }
}

beforeEach(() => {
  FakeEventSource.instances = [];
  sse.factory = (url) => new FakeEventSource(url) as unknown as EventSource;
  sse.path = '/api/stream';
});
afterEach(() => {
  sse.reset();
});

function rec(type: string, payload: unknown = {}): Partial<EventRecord> {
  return {
    event_id: 'evt-' + Math.random().toString(36).slice(2, 8),
    correlation_id: 'corr',
    causation_id: null,
    ts: '2026-05-30T00:00:00Z',
    type,
    payload,
  };
}

describe('subscribe + dispatch', () => {
  it('opens one EventSource on first subscribe and reuses it', () => {
    const h: SseHandler = vi.fn();
    sse.subscribe('agent.spawned', h);
    sse.subscribe('quota.rotated', h);
    expect(FakeEventSource.instances).toHaveLength(1);
    expect(FakeEventSource.instances[0].url).toBe('/api/stream');
  });

  it('routes by exact kind', () => {
    const seen: string[] = [];
    sse.subscribe('agent.spawned', (r) => seen.push(r.type));
    const src = FakeEventSource.instances[0];
    src.emit(rec('agent.spawned'));
    src.emit(rec('agent.killed'));
    src.emit(rec('agent.spawned'));
    expect(seen).toEqual(['agent.spawned', 'agent.spawned']);
  });

  it('routes by `domain.*` prefix', () => {
    const seen: string[] = [];
    sse.subscribe('agent.*', (r) => seen.push(r.type));
    const src = FakeEventSource.instances[0];
    src.emit(rec('agent.spawned'));
    src.emit(rec('agent.killed'));
    src.emit(rec('merge.complete'));
    expect(seen).toEqual(['agent.spawned', 'agent.killed']);
  });

  it('routes every frame for the `*` wildcard', () => {
    const seen: string[] = [];
    sse.subscribe('*', (r) => seen.push(r.type));
    const src = FakeEventSource.instances[0];
    src.emit(rec('agent.spawned'));
    src.emit(rec('merge.complete'));
    src.emit(rec('quota.rotated'));
    expect(seen).toEqual(['agent.spawned', 'merge.complete', 'quota.rotated']);
  });

  it('unsubscribe stops dispatch + closes the source when last sub leaves', () => {
    const h = vi.fn();
    const unsub = sse.subscribe('agent.*', h);
    const src = FakeEventSource.instances[0];
    expect(src.closed).toBe(false);
    unsub();
    expect(src.closed).toBe(true);
    src.emit(rec('agent.spawned'));
    expect(h).not.toHaveBeenCalled();
  });

  it('ignores un-parseable frames without throwing', () => {
    const h = vi.fn();
    sse.subscribe('*', h);
    const src = FakeEventSource.instances[0];
    src.emitRaw('not json');
    src.emit(rec('agent.spawned'));
    expect(h).toHaveBeenCalledTimes(1);
  });

  it('keeps dispatching after a subscriber throws', () => {
    const errorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
    const seen: string[] = [];
    sse.subscribe('*', () => {
      throw new Error('boom');
    });
    sse.subscribe('*', (r) => seen.push(r.type));
    FakeEventSource.instances[0].emit(rec('quota.rotated'));
    expect(seen).toEqual(['quota.rotated']);
    errorSpy.mockRestore();
  });
});

describe('status broadcast', () => {
  it('fires the current status synchronously on subscribeStatus', () => {
    const seen: SseStatus[] = [];
    sse.subscribeStatus((s) => seen.push(s));
    // First entry is the snapshot (initial `closed` from the empty router); the
    // `ensureOpen()` triggered by subscribeStatus then advances it to `connecting`.
    expect(seen[0]).toBe('closed');
    expect(seen).toContain('connecting');
  });

  it('reports `open` once EventSource fires open, then `error` on transport drop', () => {
    const seen: SseStatus[] = [];
    sse.subscribeStatus((s) => seen.push(s));
    const src = FakeEventSource.instances[0];
    src.open();
    src.error();
    expect(seen).toEqual(['closed', 'connecting', 'open', 'error']);
  });

  it('dedupes repeated identical status pushes', () => {
    const seen: SseStatus[] = [];
    sse.subscribeStatus((s) => seen.push(s));
    const src = FakeEventSource.instances[0];
    src.open();
    src.open(); // no-op
    src.error();
    src.error(); // no-op
    expect(seen).toEqual(['closed', 'connecting', 'open', 'error']);
  });
});
