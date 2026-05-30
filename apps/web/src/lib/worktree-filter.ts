// Predicate that decides whether a worktree row deserves space in the /worktrees panel
// (hq-fe-view.16). "Active" = main worktree, or one an agent owns (branch starts with
// `claim/`), or one with uncommitted changes. Everything else is "idle" — typically an
// abandoned WIP branch the operator forgot to `git worktree remove`. Idle rows are hidden
// by default so the panel stays focused on agent activity; the header counter surfaces the
// idle count so the hide isn't silent.

import type { Worktree } from '$lib/types/worktree';

export function isActive(wt: Worktree): boolean {
  if (wt.is_main) return true;
  if (wt.dirty.length > 0) return true;
  if (wt.branch?.startsWith('claim/')) return true;
  return false;
}
