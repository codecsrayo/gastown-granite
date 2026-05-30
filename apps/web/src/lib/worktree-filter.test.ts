// @vitest-environment node

import { describe, expect, it } from 'vitest';
import { isActive } from './worktree-filter';
import type { Worktree } from '$lib/types/worktree';

function mk(overrides: Partial<Worktree>): Worktree {
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

describe('isActive', () => {
  it('main worktree is always active', () => {
    expect(isActive(mk({ is_main: true, branch: 'main' }))).toBe(true);
  });

  it('claim/ branches are active even when clean', () => {
    expect(isActive(mk({ branch: 'claim/hq-fe-view-16' }))).toBe(true);
  });

  it('dirty worktrees are active even on non-claim branches', () => {
    expect(isActive(mk({ branch: 'feat/idempotency', dirty: [{ path: 'a', xy: '.M' }] }))).toBe(
      true
    );
  });

  it('clean non-claim non-main worktrees are idle', () => {
    expect(isActive(mk({ branch: 'fix/quota-5h-probe-backstop' }))).toBe(false);
    expect(isActive(mk({ branch: 'refactor/bd-jsonl-lock-race' }))).toBe(false);
  });

  it('detached HEAD with no dirty files is idle', () => {
    expect(isActive(mk({ branch: null }))).toBe(false);
  });
});
