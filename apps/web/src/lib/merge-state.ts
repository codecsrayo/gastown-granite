// Helpers for the Merge Q view (hq-fe-view.7). Lives in `$lib` so the vitest
// suite can import without bootstrapping a Svelte component runtime; the page
// component re-uses the same constants so the CSS palette and lifecycle order
// stay in lockstep with the colored chips.

/** Canonical state vocabulary in the order the operator scans. `ready` is the
 *  hot bucket (queued + waiting to merge); `merged` / `failed` are terminal. */
export const MERGE_STATE_ORDER = ['ready', 'merging', 'merged', 'failed'] as const;

export type MergeBoardState = (typeof MERGE_STATE_ORDER)[number];

/** Rank used by the board sort. Unknown states fall to the end so a future
 *  state added on the backend renders without a page crash — the row is just
 *  ordered last instead of first. */
export function mergeStateRank(state: string): number {
  const i = (MERGE_STATE_ORDER as readonly string[]).indexOf(state);
  return i === -1 ? MERGE_STATE_ORDER.length : i;
}

/** Color palette for the state chip. Matches the convention used by the
 *  Sessions / Convoys tables: `ink` = neutral hot row, `warn` = in-flight,
 *  `good` = terminal success, `bad` = terminal failure. */
export function mergeStateColor(state: string): string {
  switch (state) {
    case 'ready':
      return 'var(--ink)';
    case 'merging':
      return 'var(--warn)';
    case 'merged':
      return 'var(--good)';
    case 'failed':
      return 'var(--bad)';
    default:
      return 'var(--ink-faint)';
  }
}
