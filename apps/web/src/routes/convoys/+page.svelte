<script lang="ts">
  // hq-fe-view.6 — Convoys view. Lists the orchestrator's convoy board
  // (`/api/convoys`, hq-fe-api-r.3) grouped by state, with a per-member fail
  // DangerZone (`POST /api/convoys/:c/members/:m/fail`, hq-fe-api-w.9).
  //
  // Live updates: subscribe to `orch.*` SSE frames and refetch the snapshot on
  // any matching event so the board stays in sync without a polling timer.
  // Refetch is debounced by an in-flight flag — a burst of frames (e.g. all
  // members of a launched convoy emitting at once) collapses into one round-trip.

  import { onDestroy, onMount } from 'svelte';
  import { fetchConvoys, failConvoyMember } from '$lib/api/convoys';
  import { subscribe, subscribeStatus, type SseStatus } from '$lib/sse';
  import type { Convoy, ConvoyMember } from '$lib/types/convoy';
  import DangerZone from '$lib/components/auth/DangerZone.svelte';

  let rows = $state<Convoy[]>([]);
  let error = $state<string | null>(null);
  let stateFilter = $state<string>('');
  let status = $state<SseStatus>('closed');

  let inFlight = false;
  let pending = false;
  let unsubFrames: (() => void) | undefined;
  let unsubStatus: (() => void) | undefined;

  // Per-member modal state. Keyed by `${convoy}/${member}` so reopening for a
  // different row doesn't carry stale typed-name input.
  let confirmFor = $state<{ convoy: string; member: string } | null>(null);
  let reasonText = $state<string>('');
  let actionError = $state<string | null>(null);

  async function refresh() {
    if (inFlight) {
      pending = true;
      return;
    }
    inFlight = true;
    try {
      rows = await fetchConvoys();
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
    unsubFrames = subscribe('orch.*', () => void refresh());
    unsubStatus = subscribeStatus((s) => (status = s));
  });
  onDestroy(() => {
    unsubFrames?.();
    unsubStatus?.();
  });

  let states = $derived([...new Set(rows.map((c) => c.state))].sort());
  let visible = $derived(rows.filter((c) => !stateFilter || c.state === stateFilter));

  // Color the convoy state pill so the operator can scan the board by lifecycle
  // without reading every label. Matches the sessions table convention.
  function stateColor(s: string): string {
    if (s === 'launched') return 'var(--good)';
    if (s === 'failed') return 'var(--bad)';
    if (s === 'closed') return 'var(--ink-faint)';
    return 'var(--ink)'; // staged
  }

  function memberColor(s: string): string {
    if (s === 'active') return 'var(--good)';
    if (s === 'failed') return 'var(--bad)';
    if (s === 'done') return 'var(--ink-faint)';
    return 'var(--ink-soft)'; // pending
  }

  function memberKey(c: Convoy, m: ConvoyMember): string {
    return `${c.id}/${m.bead}`;
  }

  function openConfirm(convoy: string, member: string) {
    confirmFor = { convoy, member };
    reasonText = '';
    actionError = null;
  }

  function closeConfirm() {
    confirmFor = null;
    reasonText = '';
    actionError = null;
  }

  async function fireFail() {
    if (!confirmFor) return;
    const reason = reasonText.trim();
    if (!reason) {
      actionError = 'reason required';
      return;
    }
    const { convoy, member } = confirmFor;
    try {
      await failConvoyMember(convoy, member, reason);
      closeConfirm();
      void refresh();
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    }
  }
</script>

<svelte:head>
  <title>Convoys · Gas Town</title>
</svelte:head>

<section class="font-mono text-sm" style="color: var(--ink)">
  <header class="mb-6 flex flex-wrap items-baseline justify-between gap-3">
    <h1 class="font-sketch text-3xl" style="color: var(--accent)">Convoys</h1>
    <span class="text-xs" style="color: var(--ink-faint)">
      SSE {status} · {visible.length} of {rows.length}
    </span>
  </header>

  {#if error}
    <p class="mb-4 rounded border border-rose-500/40 bg-rose-500/10 p-3 text-rose-300">
      {error}
    </p>
  {/if}

  <div class="mb-4 flex flex-wrap items-center gap-3 text-xs">
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
  </div>

  {#if rows.length === 0 && !error}
    <p style="color: var(--ink-faint)">
      No convoys live. Launch one via MCP `orch.launch_convoy.execute` or the gt CLI.
    </p>
  {/if}

  {#each visible as c (c.id)}
    <article class="mb-6 rounded border" style="border-color: var(--border); background: var(--paper-2)">
      <header
        class="flex items-baseline justify-between border-b px-4 py-2"
        style="border-color: var(--border)"
      >
        <h2 class="font-mono" style="color: var(--ink)">{c.id}</h2>
        <span class="text-xs uppercase" style="color: {stateColor(c.state)}">{c.state}</span>
      </header>
      <table class="w-full border-separate text-left" style="border-spacing: 0">
        <thead style="color: var(--ink-faint)" class="text-[10px] uppercase">
          <tr>
            <th class="px-4 py-1">bead</th>
            <th class="px-4 py-1">state</th>
            <th class="px-4 py-1 text-right">actions</th>
          </tr>
        </thead>
        <tbody>
          {#each c.members as m (memberKey(c, m))}
            <tr>
              <td class="px-4 py-2 font-mono" style="color: var(--ink)">{m.bead}</td>
              <td class="px-4 py-2" style="color: {memberColor(m.state)}">{m.state}</td>
              <td class="px-4 py-2 text-right">
                {#if m.state === 'failed' || m.state === 'done'}
                  <span class="text-xs" style="color: var(--ink-faint)">—</span>
                {:else}
                  <button
                    type="button"
                    class="rounded border px-2 py-1 text-xs"
                    style="border-color: var(--border); color: var(--bad)"
                    onclick={() => openConfirm(c.id, m.bead)}
                  >
                    Fail…
                  </button>
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </article>
  {/each}

  {#if confirmFor}
    <DangerZone
      open={confirmFor !== null}
      name={confirmFor.member}
      title={`Fail member ${confirmFor.member}`}
      actionLabel="Fail"
      description={`Halts convoy ${confirmFor.convoy} at this member. Type the member id to confirm.`}
      onclose={closeConfirm}
      onfire={fireFail}
    />
    <div
      class="fixed inset-x-0 bottom-6 mx-auto w-full max-w-md rounded border px-4 py-3 shadow"
      style="border-color: var(--border); background: var(--paper); color: var(--ink); z-index: 60"
    >
      <label class="block text-xs" style="color: var(--ink-faint)">
        Reason (sent to audit + SSE)
      </label>
      <input
        type="text"
        bind:value={reasonText}
        placeholder="why this convoy is being halted"
        class="mt-1 w-full rounded border bg-transparent px-2 py-1 text-sm"
        style="border-color: var(--border); color: var(--ink)"
      />
      {#if actionError}
        <p class="mt-2 text-xs" style="color: var(--bad)">{actionError}</p>
      {/if}
    </div>
  {/if}
</section>
