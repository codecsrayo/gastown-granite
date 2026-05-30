// @vitest-environment node

import { describe, expect, it } from 'vitest';
import { byRecency, relativeAge } from './relative-time';
import type { Worktree } from '$lib/types/worktree';

function wt(overrides: Partial<Worktree>): Worktree {
  return {
    path: '/x',
    branch: null,
    head: '0'.repeat(40),
    is_main: false,
    ahead: 0,
    behind: 0,
    dirty: [],
    head_subject: null,
    head_author: null,
    head_time: null,
    ...overrides
  };
}

describe('relativeAge', () => {
  const NOW = 1_780_000_000;

  it('returns empty string for null', () => {
    expect(relativeAge(null, NOW)).toBe('');
  });

  it('clamps future timestamps + sub-minute deltas to "now"', () => {
    expect(relativeAge(NOW + 30, NOW)).toBe('now');
    expect(relativeAge(NOW - 10, NOW)).toBe('now');
  });

  it('formats minute / hour / day buckets', () => {
    expect(relativeAge(NOW - 5 * 60, NOW)).toBe('5m');
    expect(relativeAge(NOW - 2 * 3600, NOW)).toBe('2h');
    expect(relativeAge(NOW - 3 * 86_400, NOW)).toBe('3d');
  });

  it('falls back to ISO date past ~one month', () => {
    const fortyDaysAgo = NOW - 40 * 86_400;
    expect(relativeAge(fortyDaysAgo, NOW)).toMatch(/^\d{4}-\d{2}-\d{2}$/);
  });
});

describe('byRecency', () => {
  it('pins main to the top regardless of head_time', () => {
    const main = wt({ is_main: true, branch: 'main', head_time: 100 });
    const newer = wt({ branch: 'feat/x', head_time: 999 });
    expect([newer, main].sort(byRecency).map((w) => w.branch)).toEqual(['main', 'feat/x']);
  });

  it('orders non-main rows by head_time desc', () => {
    const old = wt({ branch: 'feat/a', head_time: 100 });
    const mid = wt({ branch: 'feat/b', head_time: 500 });
    const fresh = wt({ branch: 'feat/c', head_time: 999 });
    expect([old, mid, fresh].sort(byRecency).map((w) => w.branch)).toEqual([
      'feat/c',
      'feat/b',
      'feat/a'
    ]);
  });

  it('parks rows with null head_time at the bottom', () => {
    const unknown = wt({ branch: 'feat/x', head_time: null });
    const known = wt({ branch: 'feat/y', head_time: 5 });
    expect([unknown, known].sort(byRecency).map((w) => w.branch)).toEqual(['feat/y', 'feat/x']);
  });
});
