import { describe, expect, it } from 'vitest';
import { MERGE_STATE_ORDER, mergeStateColor, mergeStateRank } from './merge-state';

describe('merge-state', () => {
  it('rank preserves the canonical order', () => {
    const ranks = MERGE_STATE_ORDER.map((s) => mergeStateRank(s));
    expect(ranks).toEqual([0, 1, 2, 3]);
  });

  it('unknown state ranks after the canonical tail (renders, does not crash)', () => {
    expect(mergeStateRank('exploded')).toBe(MERGE_STATE_ORDER.length);
  });

  it('colors map each canonical state to a distinct palette token', () => {
    const colors = MERGE_STATE_ORDER.map((s) => mergeStateColor(s));
    expect(new Set(colors).size).toBe(MERGE_STATE_ORDER.length);
  });

  it('unknown state falls back to the faint palette token', () => {
    expect(mergeStateColor('exploded')).toBe('var(--ink-faint)');
  });
});
