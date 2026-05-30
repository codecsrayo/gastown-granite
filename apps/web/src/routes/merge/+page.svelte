<script lang="ts">
  // hq-fe-view.7 — Merge Q view. Lists the merge slot board (`/api/merges`,
  // hq-fe-api-r.4) grouped by lifecycle state with one row per slot.
  //
  // Live updates: subscribe to `merge.*` SSE frames and refetch the snapshot on
  // any matching event so the board stays in sync without a polling timer. The
  // refetch is debounced by an in-flight flag — a burst of frames (a convoy
  // landing all its members at once) collapses into one round-trip. Same pattern
  // as the Convoys view (hq-fe-view.6) — kept inline rather than extracted to a
  // shared helper while there are only two consumers.
  //
  // Read-only: the merge actor owns slot transitions and the gateway has no
  // operator override route. So the page has no DangerZone, just a board.

  import { onDestroy, onMount } from 'svelte';
  import { fetchMerges } from '$lib/api/merges';
  import { subscribe, subscribeStatus, type SseStatus } from '$lib/sse';
  import type { MergeSlot } from '$lib/types/merge';
  import {
    MERGE_STATE_ORDER,
    mergeStateColor,
    mergeStateRank
  } from '$lib/merge-state';

  let rows = $state<MergeSlot[]>([]);
  let error = $state<string | null>(null);
  let stateFilter = $state<string>('');
  let status = $state<SseStatus>('closed');

  let inFlight = false;
  let pending = false;
  let unsubFrames: (() => void) | undefined;
  let unsubStatus: (() => void) | undefined;

  async function refresh() {
    if (inFlight) {
      pending = true;
      return;
    }
    inFlight = true;
    try {
      rows = await fetchMerges();
      error = null;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      inFlight = false;
      if (pending) {
        pending = false;
        void refresh();
      }
    }
  }

  onMount(() => {
    void refresh();
    unsubFrames = subscribe('merge.*', () => void refresh());
    unsubStatus = subscribeStatus((s) => (status = s));
  });
  onDestroy(() => {
    unsubFrames?.();
    unsubStatus?.();
  });

  // The board is small (queue depth + recent history); a single sort by state
  // keeps related rows adjacent without per-state subtables. `ready` is the
  // hot bucket the operator scans first.
  let visible = $derived(
    rows
      .filter((r) => !stateFilter || r.state === stateFilter)
      .slice()
      .sort((a, b) => {
        const r = mergeStateRank(a.state) - mergeStateRank(b.state);
        return r !== 0 ? r : a.bead.localeCompare(b.bead);
      })
  );

  let states = $derived([...new Set(rows.map((r) => r.state))].sort());

  let counts = $derived(
    MERGE_STATE_ORDER.reduce<Record<string, number>>((acc, s) => {
      acc[s] = rows.filter((r) => r.state === s).length;
      return acc;
    }, {})
  );
</script>

<svelte:head>
  <title>Merge Q · Gas Town</title>
</svelte:head>

<section class="font-mono text-sm" style="color: var(--ink)">
  <header class="mb-6 flex flex-wrap items-baseline justify-between gap-3">
    <h1 class="font-sketch text-3xl" style="color: var(--accent)">Merge Q</h1>
    <span class="text-xs" style="color: var(--ink-faint)">
      SSE {status} · {visible.length} of {rows.length}
    </span>
  </header>

  {#if error}
    <p class="mb-4 rounded border border-rose-500/40 bg-rose-500/10 p-3 text-rose-300">
      {error}
    </p>
  {/if}

  <div class="mb-4 flex flex-wrap items-center gap-4 text-xs">
    <label class="flex items-center gap-2">
      <span style="color: var(--ink-faint)">state</span>
      <select
        bind:value={stateFilter}
        class="rounded border bg-transparent px-2 py-1"
        style="border-color: var(--border); color: var(--ink)"
      >
        <option value="">all</option>
        {#each states as s}
          <option value={s}>{s}</option>
        {/each}
      </select>
    </label>
    <span class="flex items-center gap-3" style="color: var(--ink-faint)">
      {#each MERGE_STATE_ORDER as s}
        <span>
          <span style="color: {mergeStateColor(s)}">●</span>
          {s} {counts[s]}
        </span>
      {/each}
    </span>
  </div>

  {#if rows.length === 0 && !error}
    <p style="color: var(--ink-faint)">
      No merges queued. Submit one via MCP `merge.submit.execute` or the gt CLI.
    </p>
  {/if}

  {#if visible.length > 0}
    <table class="w-full border-separate text-left" style="border-spacing: 0">
      <thead style="color: var(--ink-faint)" class="text-[10px] uppercase">
        <tr>
          <th class="px-3 py-2">bead</th>
          <th class="px-3 py-2">branch</th>
          <th class="px-3 py-2">state</th>
        </tr>
      </thead>
      <tbody>
        {#each visible as r (r.bead)}
          <tr class="border-t" style="border-color: var(--border)">
            <td class="px-3 py-2 font-mono" style="color: var(--ink)">{r.bead}</td>
            <td class="px-3 py-2 text-xs" style="color: var(--ink-soft)">{r.branch}</td>
            <td class="px-3 py-2 text-xs uppercase" style="color: {mergeStateColor(r.state)}">
              {r.state}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</section>
