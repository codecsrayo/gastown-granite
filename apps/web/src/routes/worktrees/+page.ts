// First-load snapshot for `/worktrees`. The page polls afterwards (see +page.svelte) so this
// loader only seeds the initial render — failures fall back to an empty list rather than
// breaking navigation; the panel surfaces the error inline.

import type { PageLoad } from './$types';
import { fetchWorktrees } from '$lib/api/worktrees';
import type { Worktree } from '$lib/types/worktree';

export const load: PageLoad = async ({ fetch }) => {
  let initial: Worktree[] = [];
  let error: string | null = null;
  try {
    initial = await fetchWorktrees(fetch);
  } catch (e) {
    error = e instanceof Error ? e.message : String(e);
  }
  return { initial, error };
};
