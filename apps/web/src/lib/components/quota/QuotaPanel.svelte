<script lang="ts">
  import { onMount } from 'svelte';
  import { fetchQuotaAccounts, fetchQuotaRotation } from '$lib/api/quota';
  import { subscribe } from '$lib/sse';
  import { quota } from '$lib/stores/quota.svelte';
  import { groupAccounts, QUOTA_GROUPS, type QuotaGroup } from '$lib/quota-meter';
  import type { QuotaRotation } from '$lib/types/quota';
  import AccountCard from './AccountCard.svelte';
  import RotationChips from './RotationChips.svelte';

  let rotation = $state<QuotaRotation>({ waiting_unlock: [], recent_rotations: [] });
  let error = $state<string | null>(null);

  let grouped = $derived(groupAccounts(quota.accounts));

  const GROUP_LABEL: Record<QuotaGroup, string> = {
    active: 'active',
    inactive: 'inactive',
    blocked: 'blocked',
  };

  async function refreshAccounts() {
    try {
      const rows = await fetchQuotaAccounts();
      quota.hydrate(rows);
      error = null;
    } catch (err) {
      error = err instanceof Error ? err.message : 'fetch failed';
    }
  }

  async function refreshRotation() {
    try {
      rotation = await fetchQuotaRotation({ limit: 16 });
    } catch {
      // Sidebar leaves the rotation strip blank rather than blocking the meters
      // when /api/quota/rotation is briefly unreachable.
    }
  }

  onMount(() => {
    refreshAccounts();
    refreshRotation();

    const offFeed = subscribe('quota.*', (rec) => {
      quota.apply(rec);
      // Cooldown + rotation entries derive from the same `quota.rotated` /
      // `account_limited` frames, so refetch the rotation snapshot on every
      // quota.* tick — the response is bounded (16 rows + cooldown registry).
      refreshRotation();
    });

    return () => offFeed();
  });
</script>

<section
  class="flex flex-col gap-2 px-3 py-3 font-mono text-[11px]"
  style:border-top="1px solid var(--border-soft)"
  style:background="var(--paper)"
  data-testid="quota-panel"
  aria-label="Account quota"
>
  <header
    class="flex items-baseline justify-between"
    style="color: var(--ink-faint)"
  >
    <span class="uppercase tracking-wide">Quota</span>
    <span class="text-[10px]" title="hq-fe-view.10">{quota.accounts.length}</span>
  </header>

  {#if error}
    <p
      class="rounded px-1.5 py-0.5 text-[10px]"
      style:color="var(--bad)"
      style:background="var(--bad-soft)"
      data-testid="quota-error"
    >
      {error}
    </p>
  {/if}

  {#if quota.accounts.length === 0 && !error}
    <p class="text-[10px]" style="color: var(--ink-faint)">
      no accounts registered
    </p>
  {:else}
    {#each QUOTA_GROUPS as group (group)}
      {#if grouped[group].length > 0}
        <div class="flex flex-col gap-1.5">
          <span
            class="text-[10px] uppercase tracking-wide"
            style="color: var(--ink-faint)"
          >
            {GROUP_LABEL[group]}
          </span>
          {#each grouped[group] as account (account.id)}
            <AccountCard {account} />
          {/each}
        </div>
      {/if}
    {/each}
  {/if}

  <div class="mt-1 flex flex-col gap-1">
    <span
      class="text-[10px] uppercase tracking-wide"
      style="color: var(--ink-faint)"
    >
      rotation
    </span>
    <RotationChips {rotation} />
  </div>
</section>
