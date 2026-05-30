// First-load snapshot for `/crew`. Same shape as `/sessions` (hq-fe-view.4): the
// canonical role catalog is static (six roles defined in frontend-features.md §8),
// but the live count + bound crews per role are derived from `/api/sessions`. The
// real `GET /api/roles` + `GET /api/skills` ship with hq-fe-skills.2; until then the
// SkillToggle + ScopeMatrix panels render as disabled placeholders, and the page
// polls /api/sessions every 3s to keep the per-role roster fresh.

import type { PageLoad } from './$types';
import { fetchSessions } from '$lib/api/sessions';
import type { Session } from '$lib/types/session';

export const load: PageLoad = async ({ fetch }) => {
  let initial: Session[] = [];
  let error: string | null = null;
  try {
    initial = await fetchSessions(undefined, { fetchFn: fetch });
  } catch (e) {
    error = e instanceof Error ? e.message : String(e);
  }
  return { initial, error };
};
