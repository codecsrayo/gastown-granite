// Quota store · runes singleton (hq-fe-build.4).
//
// Mirror of `GET /api/quota/accounts` (hq-fe-api-r.1, currently open) + the
// `quota.*` slice of the SSE bus. Drives the left-rail Quota sidebar
// (hq-fe-view.10). Today the backend endpoint isn't shipped, so `hydrate`
// is a no-op until r.1 lands — the store still exists so views can bind
// against it and `apply()` can fold real `quota.tokens_sampled` /
// `.window_reset` / `.rotated` frames the moment they flow.

import type { EventRecord } from '$lib/types/event';
import type { QuotaAccount } from '$lib/types/quota';

class Quota {
  accounts = $state<QuotaAccount[]>([]);

  hydrate(initial: QuotaAccount[]): void {
    this.accounts = [...initial];
  }

  /** Fold one `quota.*` frame into the live state. Unknown kinds are dropped silently —
   *  the backend's event taxonomy can grow without the store learning every new shape. */
  apply(rec: EventRecord): void {
    if (!rec.type.startsWith('quota.')) return;
    const p = rec.payload as Record<string, unknown>;
    const id = typeof p?.account === 'string' ? p.account : undefined;
    if (!id) return;
    switch (rec.type) {
      case 'quota.tokens_sampled':
        this.patch(id, {
          tokens_used: typeof p.tokens_used === 'number' ? p.tokens_used : undefined,
        });
        return;
      case 'quota.window_reset':
        this.patch(id, {
          tokens_used: 0,
          reset_at: typeof p.reset_at === 'number' ? p.reset_at : undefined,
        });
        return;
      case 'quota.account_limited':
        this.patch(id, { state: 'blocked' });
        return;
      case 'quota.rotated':
        // Just a transition event; the new live account is on the row payload, the old
        // one is "parked". Leave the per-row state column to the next `account_limited` /
        // `tokens_sampled` frame so the store doesn't second-guess the actor.
        return;
      default:
        return;
    }
  }

  byId(id: string): QuotaAccount | undefined {
    return this.accounts.find((a) => a.id === id);
  }

  reset(): void {
    this.accounts = [];
  }

  private patch(id: string, fields: Partial<QuotaAccount>): void {
    const filtered: Partial<QuotaAccount> = {};
    for (const [k, v] of Object.entries(fields)) {
      if (v !== undefined) (filtered as Record<string, unknown>)[k] = v;
    }
    this.accounts = this.accounts.map((a) => (a.id === id ? { ...a, ...filtered } : a));
  }
}

export const quota = new Quota();
