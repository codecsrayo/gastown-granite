<script lang="ts">
  import type { QuotaAccount } from '$lib/types/quota';
  import QuotaMeter from './QuotaMeter.svelte';
  import LoginBtn from './LoginBtn.svelte';

  interface Props {
    account: QuotaAccount;
    nowSecs?: number;
  }

  let { account, nowSecs }: Props = $props();

  let stateColor = $derived(
    account.state === 'blocked'
      ? 'var(--bad)'
      : account.state === 'inactive'
        ? 'var(--ink-faint)'
        : 'var(--accent)'
  );
</script>

<article
  class="flex flex-col gap-1.5 rounded border px-2 py-1.5"
  style:border-color="var(--border-soft)"
  style:background="var(--paper-2)"
  data-testid="quota-account-card"
  data-account={account.id}
>
  <header class="flex items-baseline justify-between gap-2">
    <span class="font-mono text-xs" style="color: var(--ink)" title={account.id}>
      {account.id}
    </span>
    <span
      class="font-mono text-[10px] uppercase tracking-wide"
      style:color={stateColor}
    >
      {account.state}
    </span>
  </header>

  <QuotaMeter {account} {nowSecs} />

  {#if account.sessions.length > 0}
    <ul
      class="flex flex-wrap gap-1 font-mono text-[10px]"
      style="color: var(--ink-soft)"
    >
      {#each account.sessions as session (session)}
        <li
          class="rounded px-1"
          style:background="var(--border-soft)"
          title={`session ${session}`}
        >
          {session}
        </li>
      {/each}
    </ul>
  {/if}

  <div class="flex justify-end">
    <LoginBtn account={account.id} />
  </div>
</article>
