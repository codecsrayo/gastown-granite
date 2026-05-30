import { describe, expect, it } from 'vitest';
import {
  KANBAN_COLUMNS,
  isBeadStatus,
  isTransitionAllowed,
  type BeadStatus
} from './kanban';

describe('kanban state machine', () => {
  it('exposes the canonical 5-column order', () => {
    expect(KANBAN_COLUMNS).toEqual(['pending', 'dispatched', 'working', 'done', 'failed']);
  });

  it('rejects self-transitions', () => {
    for (const s of KANBAN_COLUMNS) {
      expect(isTransitionAllowed(s, s)).toBe(false);
    }
  });

  it('matches the gt-web operator matrix exactly', () => {
    const cases: Array<[BeadStatus, BeadStatus, boolean]> = [
      ['pending', 'working', true],
      ['pending', 'done', true],
      ['pending', 'failed', true],
      ['pending', 'dispatched', false],
      ['dispatched', 'pending', true],
      ['dispatched', 'failed', true],
      ['dispatched', 'working', false],
      ['working', 'pending', true],
      ['working', 'done', true],
      ['working', 'failed', true],
      ['done', 'pending', true],
      ['done', 'working', false],
      ['done', 'failed', false],
      ['failed', 'pending', true],
      ['failed', 'done', false]
    ];
    for (const [from, to, expected] of cases) {
      expect(isTransitionAllowed(from, to), `${from}→${to}`).toBe(expected);
    }
  });

  it('isBeadStatus narrows valid strings', () => {
    expect(isBeadStatus('working')).toBe(true);
    expect(isBeadStatus('archived')).toBe(false);
    expect(isBeadStatus('')).toBe(false);
  });
});
