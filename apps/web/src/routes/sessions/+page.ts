// First-load snapshot for `/sessions`. The page polls afterwards (no sessions SSE channel
// yet — `agent.*` records flow through `/api/stream` but need a projection layer the FE
// hasn't built). On fetch failure we fall through to an empty list so the panel still
// mounts and surfaces the error inline.

import type { PageLoad } from './$types';
import { fetchSessions } from '$lib/api/sessions';
import type { Session } from '$lib/types/session';

export const load: PageLoad = async ({ fetch }) => {
  let initial: Session[] = [];
  let error: string | null = null;
  try {
    initial = await fetchSessions(undefined, fetch);
  } catch (e) {
    error = e instanceof Error ? e.message : String(e);
  }
  return { initial, error };
};
