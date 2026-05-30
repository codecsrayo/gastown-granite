// Maps a wire `EventRecord.type` (e.g. `agent.spawned`, `web.invoked`, `quota.rotated`) to
// the five buckets the Activity tab's category filter exposes (hq-fe-view.3). The buckets
// are picked from the canon listing in `frontend-features.md §3`:
//
//   agent   — agent.* (spawned, killed, heartbeat, session_end, transition, scope)
//   work    — merge.* + patrol.* + orch.* + scheduling.* (everything bead/convoy lifecycle)
//   quota   — quota.* (sampled, rotated, account_limited, window_reset, login_*)
//   system  — rig.* + platform.* + anything domain-y that doesn't fit elsewhere
//   audit   — web.* + mcp.* (frontier-audit who-consulted-what)
//
// Unknown prefixes fall through to `system` so a freshly-added wire kind doesn't vanish
// from the feed silently — operators see it under "system" until a curator routes it.
//
// Pure logic. Tested in `event-category.test.ts`.

export type Category = 'agent' | 'work' | 'quota' | 'system' | 'audit';
export const CATEGORIES: readonly Category[] = ['agent', 'work', 'quota', 'system', 'audit'];

export function categoryOf(kind: string): Category {
  if (kind.startsWith('agent.')) return 'agent';
  if (
    kind.startsWith('merge.') ||
    kind.startsWith('patrol.') ||
    kind.startsWith('orch.') ||
    kind.startsWith('scheduling.')
  ) {
    return 'work';
  }
  if (kind.startsWith('quota.')) return 'quota';
  if (kind.startsWith('web.') || kind.startsWith('mcp.')) return 'audit';
  return 'system';
}
