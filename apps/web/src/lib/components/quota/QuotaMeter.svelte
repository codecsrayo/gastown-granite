<script lang="ts">
  import type { QuotaAccount } from '$lib/types/quota';
  import { meterRatio, meterTone, resetCountdown } from '$lib/quota-meter';

  interface Props {
    account: QuotaAccount;
    /** Pinned clock (for tests). Defaults to wall time. */
    nowSecs?: number;
  }

  let { account, nowSecs }: Props = $props();

  let tone = $derived(meterTone(account));
  let ratio = $derived(meterRatio(account));
  let countdown = $derived(resetCountdown(account.reset_at, nowSecs));

  let barColor = $derived(
    tone === 'bad'
      ? 'var(--bad)'
      : tone === 'warn'
        ? 'var(--warn)'
        : tone === 'good'
          ? 'var(--accent)'
          : 'var(--border)'
  );
  let trackColor = $derived(
    tone === 'bad'
      ? 'var(--bad-soft)'
      : tone === 'warn'
        ? 'var(--warn-soft)'
        : 'var(--border-soft)'
  );

  function fmtTokens(n: number | null): string {
    if (n === null) return '—';
    if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
    if (n >= 1_000) return `${(n / 1_000).toFixed(0)}k`;
    return String(n);
  }
</script>

<div class="flex flex-col gap-1" data-testid="quota-meter" data-tone={tone}>
  <div
    class="relative h-1.5 w-full overflow-hidden rounded"
    style:background={trackColor}
  >
    <div
      class="absolute inset-y-0 left-0 transition-all"
      style:width={`${Math.round(ratio * 100)}%`}
      style:background={barColor}
      aria-label="tokens used"
    ></div>
  </div>
  <div
    class="flex items-baseline justify-between font-mono text-[10px]"
    style="color: var(--ink-faint)"
  >
    <span>
      {fmtTokens(account.tokens_used)} / {fmtTokens(account.tokens_cap)}
    </span>
    {#if countdown}
      <span title="window resets in {countdown}">reset {countdown}</span>
    {/if}
  </div>
</div>
