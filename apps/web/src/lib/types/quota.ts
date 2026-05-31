// Wire shape for `GET /api/quota/accounts` (hq-fe-api-r.1, currently open). The endpoint
// is not yet shipped on the backend; this type captures the shape the dashboard plans to
// consume so the store + the sidebar component (hq-fe-view.10) can be written against it.
// Fields will land in the gt-web DTO when the bead closes; the wire ↔ TS lockstep mirror
// (same nullability) is the contract the sidebar relies on.
export interface QuotaAccount {
  /** Stable account id (e.g. `brayan`, `fsrb`, `codecsrayo`, `a407`). */
  id: string;
  state: 'active' | 'inactive' | 'blocked';
  /** Tokens consumed in the current 5h window. `null` until the first sample. */
  tokens_used: number | null;
  /** Hard cap for the window. `null` until the live-window probe lands. */
  tokens_cap: number | null;
  /** Unix seconds when the current window resets. `null` for accounts with no live window. */
  reset_at: number | null;
  /** Sessions currently pinned to this account (debug + ops display). */
  sessions: string[];
}

// Wire shapes for `GET /api/quota/rotation` (hq-fe-api-r.2). Composite snapshot for the
// rotation panel: every account currently in cooldown plus the tail of `quota.rotated`
// log records. Mirrors `QuotaRotationDto` in `gt-web::dto`.

/** One row of `waiting_unlock`. `unlock_at_secs` is the rolling-5h boundary the cooldown
 *  expires against (mirror of `account.window.resets_at_secs`); `null` when the account
 *  has no live window yet. */
export interface QuotaWaitingUnlock {
  account: string;
  status: string;
  unlock_at_secs: number | null;
}

/** One row of `recent_rotations`. `ts` is the RFC3339 timestamp of the source
 *  `quota.rotated` `EventRecord`. */
export interface QuotaRotationEntry {
  from: string;
  to: string;
  ts: string;
}

export interface QuotaRotation {
  waiting_unlock: QuotaWaitingUnlock[];
  recent_rotations: QuotaRotationEntry[];
}

// Write-side wire shapes (hq-fe-api-w.10). Promotes `quota.rotate` / `quota.retire`
// from MCP-only to the HTTP gateway. Mirrors `gt_web::dto`.

/** Body of `POST /api/quota/accounts/:id/rotate`. Path `:id` is the source account;
 *  `to_account` is the healthy target. `now_secs` is optional — when absent the
 *  gateway stamps `SystemTime::now()` so curl smoke tests stay one-liners. */
export interface QuotaRotateRequest {
  to_account: string;
  now_secs?: number;
}

/** Response of `POST /api/quota/accounts/:id/retire`. `removed: false` when the id
 *  was already absent — the route is idempotent (200, never 404). */
export interface QuotaRetireResponse {
  account: string;
  removed: boolean;
}
