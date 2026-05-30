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
