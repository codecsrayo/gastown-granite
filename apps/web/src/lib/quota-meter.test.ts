import { describe, expect, it } from 'vitest';
import type { QuotaAccount } from '$lib/types/quota';
import {
  groupAccounts,
  meterRatio,
  meterTone,
  resetCountdown,
  type MeterTone,
} from './quota-meter';

function acc(over: Partial<QuotaAccount> = {}): QuotaAccount {
  return {
    id: 'a',
    state: 'active',
    tokens_used: null,
    tokens_cap: null,
    reset_at: null,
    sessions: [],
    ...over,
  };
}

describe('meterTone', () => {
  const cases: Array<[QuotaAccount, MeterTone]> = [
    [acc(), 'idle'],
    [acc({ tokens_used: 0, tokens_cap: 1000 }), 'good'],
    [acc({ tokens_used: 740, tokens_cap: 1000 }), 'good'],
    [acc({ tokens_used: 750, tokens_cap: 1000 }), 'warn'],
    [acc({ tokens_used: 999, tokens_cap: 1000 }), 'warn'],
    [acc({ state: 'blocked', tokens_used: 100, tokens_cap: 1000 }), 'bad'],
    [acc({ tokens_used: 999, tokens_cap: 0 }), 'idle'],
  ];
  it.each(cases)('classifies %j -> %s', (a, expected) => {
    expect(meterTone(a)).toBe(expected);
  });
});

describe('meterRatio', () => {
  it('caps at 1 even when over budget', () => {
    expect(meterRatio({ tokens_used: 2000, tokens_cap: 1000 })).toBe(1);
  });
  it('returns 0 when window is missing', () => {
    expect(meterRatio({ tokens_used: null, tokens_cap: null })).toBe(0);
  });
});

describe('resetCountdown', () => {
  it('formats the four buckets + now', () => {
    expect(resetCountdown(null)).toBe('');
    expect(resetCountdown(0)).toBe('');
    expect(resetCountdown(100, 100)).toBe('now');
    expect(resetCountdown(100, 150)).toBe('now'); // already passed
    expect(resetCountdown(100, 70)).toBe('30s');
    expect(resetCountdown(1000, 100)).toBe('15m');
    expect(resetCountdown(7200 + 100, 100)).toBe('2h');
    expect(resetCountdown(3 * 86400 + 100, 100)).toBe('3d');
  });
});

describe('groupAccounts', () => {
  it('buckets by state preserving order', () => {
    const out = groupAccounts([
      acc({ id: 'a', state: 'active' }),
      acc({ id: 'b', state: 'blocked' }),
      acc({ id: 'c', state: 'inactive' }),
      acc({ id: 'd', state: 'active' }),
    ]);
    expect(out.active.map((x) => x.id)).toEqual(['a', 'd']);
    expect(out.inactive.map((x) => x.id)).toEqual(['c']);
    expect(out.blocked.map((x) => x.id)).toEqual(['b']);
  });
});
