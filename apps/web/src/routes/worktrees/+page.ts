// First-load snapshot for `/worktrees`. The page polls afterwards (see +page.svelte) so this
// loader only seeds the initial render — failures fall back to empty lists rather than
// breaking navigation; the panel surfaces the error inline.
//
// Two independent fetches go out in parallel (Promise.allSettled, not Promise.all): if the
// issues call fails we still want the worktree list to render — the cross-link is
// enrichment, not the primary data (hq-fe-view.15).

import type { PageLoad } from './$types';
import { fetchWorktrees } from '$lib/api/worktrees';
import { fetchIssues } from '$lib/api/issues';
import type { Worktree } from '$lib/types/worktree';
import type { Issue } from '$lib/types/issue';

export const load: PageLoad = async ({ fetch }) => {
  const [wtResult, issuesResult] = await Promise.allSettled([
    fetchWorktrees(fetch),
    fetchIssues('open,working', fetch),
  ]);

  const worktrees: Worktree[] = wtResult.status === 'fulfilled' ? wtResult.value : [];
  const issues: Issue[] = issuesResult.status === 'fulfilled' ? issuesResult.value : [];
  const error: string | null =
    wtResult.status === 'rejected' ? String(wtResult.reason) : null;

  return { initial: worktrees, issues, error };
};
