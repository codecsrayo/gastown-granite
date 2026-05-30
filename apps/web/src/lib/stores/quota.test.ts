import { afterEach, describe, expect, it } from 'vitest';
import type { EventRecord } from '$lib/types/event';
import type { QuotaAccount } from '$lib/types/quota';
import { quota } from './quota.svelte';

function acc(id: string, state: QuotaAccount['state'] = 'active'): QuotaAccount {
  return { id, state, tokens_used: null, tokens_cap: null, reset_at: null, sessions: [] };
}

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

afterEach(() => quota.reset());

describe('quota store', () => {
  it('hydrate copies the snapshot + byId resolves', () => {
    quota.hydrate([acc('brayan'), acc('fsrb', 'inactive')]);
    expect(quota.accounts).toHaveLength(2);
    expect(quota.byId('fsrb')?.state).toBe('inactive');
  });

  it('quota.tokens_sampled patches the row', () => {
    quota.hydrate([acc('brayan')]);
    quota.apply(ev('quota.tokens_sampled', { account: 'brayan', tokens_used: 1234 }));
    expect(quota.byId('brayan')?.tokens_used).toBe(1234);
  });

  it('quota.window_reset zeroes usage + stamps reset_at', () => {
    quota.hydrate([{ ...acc('brayan'), tokens_used: 9000 }]);
    quota.apply(ev('quota.window_reset', { account: 'brayan', reset_at: 1_780_000_000 }));
    expect(quota.byId('brayan')?.tokens_used).toBe(0);
    expect(quota.byId('brayan')?.reset_at).toBe(1_780_000_000);
  });

  it('quota.account_limited flips state to blocked', () => {
    quota.hydrate([acc('brayan')]);
    quota.apply(ev('quota.account_limited', { account: 'brayan' }));
    expect(quota.byId('brayan')?.state).toBe('blocked');
  });

  it('non quota.* frames are ignored', () => {
    quota.hydrate([acc('brayan')]);
    const before = quota.accounts;
    quota.apply(ev('agent.spawned', { account: 'brayan' }));
    expect(quota.accounts).toBe(before);
  });

  it('quota.rotated is a no-op for per-row state', () => {
    quota.hydrate([acc('brayan'), acc('fsrb', 'inactive')]);
    quota.apply(ev('quota.rotated', { account: 'brayan' }));
    expect(quota.byId('brayan')?.state).toBe('active');
    expect(quota.byId('fsrb')?.state).toBe('inactive');
  });

  it('frames without an account id are dropped', () => {
    quota.hydrate([acc('brayan')]);
    const before = quota.accounts;
    quota.apply(ev('quota.tokens_sampled', { tokens_used: 42 }));
    expect(quota.accounts).toBe(before);
  });
});
