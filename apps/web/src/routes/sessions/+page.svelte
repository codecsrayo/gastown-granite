<script lang="ts">
  // hq-fe-view.4 — Sessions table. Lists the live polecat/dog/mayor registry from
  // `/api/sessions` (hq-fe-api-r reader). Polls every 3s; no SSE yet because the
  // sessions projection over `/api/stream`'s `agent.*` records hasn't been plumbed.
  //
  // Kill column is a disabled DangerButton placeholder — the underlying API route
  // `DELETE /api/sessions/:id` (hq-fe-api-w.6) is being claimed by another agent.
  // The button activates the moment that bead lands and the route returns 2xx.

  import { onDestroy, onMount } from 'svelte';
  import { fetchSessions } from '$lib/api/sessions';
  import type { Session } from '$lib/types/session';
  import DangerButton from '$lib/components/auth/DangerButton.svelte';

  let { data } = $props<{ data: { initial: Session[]; error: string | null } }>();

  let rows = $state<Session[]>([]);
  let error = $state<string | null>(null);
  $effect(() => {
    rows = data.initial;
    error = data.error;
  });

  // Filters are derived state — operator toggles narrow the visible slice without
  // re-fetching, so the live snapshot stays in sync across all chips.
  let roleFilter = $state<string>('');
  let rigFilter = $state<string>('');
  let stateFilter = $state<string>('');

  let roles = $derived(unique(rows.map((r) => r.role)));
  let rigs = $derived(unique(rows.map((r) => r.rig)));
  let states = $derived(unique(rows.map((r) => r.state)));

  let visible = $derived(
    rows.filter(
      (s) =>
        (!roleFilter || s.role === roleFilter) &&
        (!rigFilter || s.rig === rigFilter) &&
        (!stateFilter || s.state === stateFilter),
    ),
  );

  function unique(xs: string[]): string[] {
    return [...new Set(xs)].sort();
  }

  let timer: ReturnType<typeof setInterval> | undefined;

  async function refresh() {
    try {
      rows = await fetchSessions();
      error = null;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  onMount(() => {
    timer = setInterval(refresh, 3000);
  });
  onDestroy(() => {
    if (timer) clearInterval(timer);
  });

  // Role tinting matches the canon docs (polecats neutral, dogs accent, mayor warn) so
  // the operator can scan the column at a glance without reading every label.
  function roleColor(role: string): string {
    if (role === 'mayor') return 'var(--warn)';
    if (role === 'polecat') return 'var(--ink-soft)';
    return 'var(--accent)'; // sheriff / deacon / refinery / witness — dog roles
  }

  function stateColor(state: string): string {
    if (state === 'working') return 'var(--good)';
    if (state === 'killed') return 'var(--bad)';
    if (state === 'done') return 'var(--ink-faint)';
    return 'var(--ink)';
  }
</script>

<svelte:head>
  <title>Sessions · Gas Town</title>
</svelte:head>

<section class="font-mono text-sm" style="color: var(--ink)">
  <header class="mb-6 flex flex-wrap items-baseline justify-between gap-3">
    <h1 class="font-sketch text-3xl" style="color: var(--accent)">Sessions</h1>
    <span class="text-xs" style="color: var(--ink-faint)">
      polling every 3s · {visible.length} of {rows.length}
    </span>
  </header>

  {#if error}
    <p class="mb-4 rounded border border-rose-500/40 bg-rose-500/10 p-3 text-rose-300">
      {error}
    </p>
  {/if}

  <div class="mb-4 flex flex-wrap items-center gap-3 text-xs">
    <label class="flex items-center gap-2">
      <span style="color: var(--ink-faint)">role</span>
      <select bind:value={roleFilter} class="rounded border bg-transparent px-2 py-1"
        style="border-color: var(--border); color: var(--ink)">
        <option value="">all</option>
        {#each roles as r}
          <option value={r}>{r}</option>
        {/each}
      </select>
    </label>
    <label class="flex items-center gap-2">
      <span style="color: var(--ink-faint)">rig</span>
      <select bind:value={rigFilter} class="rounded border bg-transparent px-2 py-1"
        style="border-color: var(--border); color: var(--ink)">
        <option value="">all</option>
        {#each rigs as r}
          <option value={r}>{r}</option>
        {/each}
      </select>
    </label>
    <label class="flex items-center gap-2">
      <span style="color: var(--ink-faint)">state</span>
      <select bind:value={stateFilter} class="rounded border bg-transparent px-2 py-1"
        style="border-color: var(--border); color: var(--ink)">
        <option value="">all</option>
        {#each states as s}
          <option value={s}>{s}</option>
        {/each}
      </select>
    </label>
  </div>

  {#if rows.length === 0 && !error}
    <p style="color: var(--ink-faint)">
      No sessions live. Spawn a polecat or wait for the orchestrator to claim work.
    </p>
  {/if}

  {#if visible.length > 0}
    <table class="w-full border-separate text-left" style="border-spacing: 0 4px">
      <thead style="color: var(--ink-faint)" class="text-[10px] uppercase">
        <tr>
          <th class="px-3 py-1">id</th>
          <th class="px-3 py-1">rig</th>
          <th class="px-3 py-1">role</th>
          <th class="px-3 py-1">crew</th>
          <th class="px-3 py-1">state</th>
          <th class="px-3 py-1 text-right">actions</th>
        </tr>
      </thead>
      <tbody>
        {#each visible as s (s.id)}
          <tr style="background: var(--paper-2)">
            <td class="px-3 py-2 font-mono" style="color: var(--ink)">{s.id}</td>
            <td class="px-3 py-2" style="color: var(--ink-soft)">{s.rig}</td>
            <td class="px-3 py-2" style="color: {roleColor(s.role)}">{s.role}</td>
            <td class="px-3 py-2" style="color: var(--ink-soft)">{s.crew ?? '—'}</td>
            <td class="px-3 py-2" style="color: {stateColor(s.state)}">{s.state}</td>
            <td class="px-3 py-2 text-right">
              <DangerButton
                label="Kill"
                armedLabel="Confirm kill"
                disabled
                onfire={() => {
                  // DELETE /api/sessions/:id lands with hq-fe-api-w.6; until then the
                  // button stays disabled so a stale UI never claims to have killed.
                }}
              />
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</section>
