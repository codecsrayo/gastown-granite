// Short relative-time formatter for the /worktrees panel (hq-fe-view.18). Renders a Unix
// timestamp as "now", "5m", "2h", "3d", or an ISO date once we cross a month — same shape
// VSCode's git history uses, optimised for column width over verbosity. `nowSecs` is
// injectable so the vitest suite can pin a deterministic clock.
//
// Returns `''` for null inputs so the caller can render the chip conditionally without
// guarding on the timestamp at the call site.
export function relativeAge(
  thenSecs: number | null,
  nowSecs: number = Math.floor(Date.now() / 1000),
): string {
  if (thenSecs === null || thenSecs <= 0) return '';
  const delta = nowSecs - thenSecs;
  if (delta < 0) return 'now'; // clock skew — treat future as "now" rather than negative
  if (delta < 60) return 'now';
  if (delta < 3600) return `${Math.floor(delta / 60)}m`;
  if (delta < 86_400) return `${Math.floor(delta / 3600)}h`;
  if (delta < 30 * 86_400) return `${Math.floor(delta / 86_400)}d`;
  // Older than ~a month: full date in ISO yyyy-mm-dd. The chip stays readable and the
  // operator sees the actual age, not "very long ago".
  return new Date(thenSecs * 1000).toISOString().slice(0, 10);
}

// Sort comparator for `Worktree[]`. Main worktree always pinned to the top; the rest sort
// by `head_time` descending so the most recently touched bubbles up. Worktrees with no
// `head_time` (unreadable HEAD) drop to the bottom of the non-main slice, keeping the
// "what was just done" axis monotonic.
import type { Worktree } from '$lib/types/worktree';

export function byRecency(a: Worktree, b: Worktree): number {
  if (a.is_main && !b.is_main) return -1;
  if (!a.is_main && b.is_main) return 1;
  const at = a.head_time ?? -1;
  const bt = b.head_time ?? -1;
  return bt - at;
}
