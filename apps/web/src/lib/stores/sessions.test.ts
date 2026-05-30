import { afterEach, describe, expect, it } from 'vitest';
import type { EventRecord } from '$lib/types/event';
import { sessions } from './sessions.svelte';

function ev(type: string, payload: Record<string, unknown>): EventRecord {
  return {
    event_id: 'e' + Math.random().toString(36).slice(2, 8),
    correlation_id: 'c',
    causation_id: null,
    ts: '2026-05-30T00:00:00Z',
    type,
    payload,
  };
}

afterEach(() => sessions.reset());

describe('sessions store', () => {
  it('hydrate copies the initial slice', () => {
    sessions.hydrate([{ id: 'a', rig: 'r', state: 'working', role: 'polecat', crew: null }]);
    expect(sessions.rows).toHaveLength(1);
    expect(sessions.byId('a')?.state).toBe('working');
  });

  it('agent.spawned upserts the row', () => {
    sessions.apply(ev('agent.spawned', { session: 'p1', rig: 'hq', role: 'polecat', crew: 'host' }));
    expect(sessions.rows).toHaveLength(1);
    expect(sessions.byId('p1')).toMatchObject({
      id: 'p1',
      state: 'spawned',
      role: 'polecat',
      crew: 'host',
    });
  });

  it('agent.transition updates state in place', () => {
    sessions.apply(ev('agent.spawned', { session: 'p1', rig: 'hq', role: 'polecat' }));
    sessions.apply(ev('agent.transition', { session: 'p1', to: 'working' }));
    expect(sessions.byId('p1')?.state).toBe('working');
    expect(sessions.rows).toHaveLength(1);
  });

  it('agent.session_end removes the row', () => {
    sessions.apply(ev('agent.spawned', { session: 'p1', rig: 'hq', role: 'polecat' }));
    sessions.apply(ev('agent.session_end', { session: 'p1' }));
    expect(sessions.rows).toHaveLength(0);
  });

  it('agent.killed removes the row even on partial payloads', () => {
    sessions.apply(ev('agent.spawned', { session: 'p1', rig: 'hq', role: 'polecat' }));
    sessions.apply(ev('agent.killed', { session: 'p1' }));
    expect(sessions.byId('p1')).toBeUndefined();
  });

  it('agent.heartbeat is a no-op for the rows', () => {
    sessions.apply(ev('agent.spawned', { session: 'p1', rig: 'hq', role: 'polecat' }));
    const before = sessions.rows;
    sessions.apply(ev('agent.heartbeat', { session: 'p1' }));
    expect(sessions.rows).toBe(before);
  });

  it('non-agent.* frames are ignored', () => {
    sessions.apply(ev('agent.spawned', { session: 'p1', rig: 'hq', role: 'polecat' }));
    sessions.apply(ev('merge.complete', { sha: 'abc' }));
    sessions.apply(ev('quota.tokens_sampled', { account: 'x' }));
    expect(sessions.rows).toHaveLength(1);
  });

  it('payloads without a session id are dropped', () => {
    sessions.hydrate([{ id: 'p1', rig: 'hq', state: 'working', role: 'polecat', crew: null }]);
    sessions.apply(ev('agent.spawned', { rig: 'hq' }));
    expect(sessions.rows).toHaveLength(1);
  });
});
