// Pure helpers for the Quota sidebar (hq-fe-view.10). Kept outside the .svelte
// components so the meter math + countdown formatting are unit-testable without
// JSDOM. Mirror of the threshold table documented in `frontend-features.md` §2:
// `warn` > 75%, `bad` when the account is rate-limited (the row carries
// `state: 'blocked'`, not a numeric signal).

import type { QuotaAccount } from '$lib/types/quota';

export type MeterTone = 'good' | 'warn' | 'bad' | 'idle';

/** Pick the tone the meter renders. `idle` is the no-window posture (e.g. before
 *  the first `quota.tokens_sampled` for the account). */
export function meterTone(account: Pick<QuotaAccount, 'state' | 'tokens_used' | 'tokens_cap'>): MeterTone {
  if (account.state === 'blocked') return 'bad';
  if (account.tokens_used === null || account.tokens_cap === null || account.tokens_cap === 0) {
    return 'idle';
  }
  const pct = account.tokens_used / account.tokens_cap;
  if (pct >= 0.75) return 'warn';
  return 'good';
}

/** Bar fill ratio in [0, 1]. Caps at 1 so the bar never overflows visually. */
export function meterRatio(
  account: Pick<QuotaAccount, 'tokens_used' | 'tokens_cap'>
): number {
  if (account.tokens_used === null || account.tokens_cap === null || account.tokens_cap === 0) {
    return 0;
  }
  return Math.max(0, Math.min(1, account.tokens_used / account.tokens_cap));
}

/** Render a Unix-seconds reset boundary as a short countdown ("3h", "12m", "now"). Returns
 *  `''` for null inputs so the chip can be rendered conditionally without a call-site guard. */
export function resetCountdown(
  resetAtSecs: number | null,
  nowSecs: number = Math.floor(Date.now() / 1000)
): string {
  if (resetAtSecs === null || resetAtSecs <= 0) return '';
  const delta = resetAtSecs - nowSecs;
  if (delta <= 0) return 'now';
  if (delta < 60) return `${delta}s`;
  if (delta < 3600) return `${Math.floor(delta / 60)}m`;
  if (delta < 86_400) return `${Math.floor(delta / 3600)}h`;
  return `${Math.floor(delta / 86_400)}d`;
}

/** Bucket an account into the three sidebar groups (`active` / `inactive` / `blocked`).
 *  Order is the canonical visual order the panel renders. */
export const QUOTA_GROUPS = ['active', 'inactive', 'blocked'] as const;
export type QuotaGroup = (typeof QUOTA_GROUPS)[number];

export function groupAccounts(accounts: QuotaAccount[]): Record<QuotaGroup, QuotaAccount[]> {
  const out: Record<QuotaGroup, QuotaAccount[]> = { active: [], inactive: [], blocked: [] };
  for (const a of accounts) {
    const g = (QUOTA_GROUPS as readonly string[]).includes(a.state)
      ? (a.state as QuotaGroup)
      : 'inactive';
    out[g].push(a);
  }
  return out;
}
