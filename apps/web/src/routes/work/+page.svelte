<script lang="ts">
  import { onMount } from 'svelte';
  import { dndzone, type DndEvent } from 'svelte-dnd-action';
  import { flip } from 'svelte/animate';
  import Guard from '$lib/components/auth/Guard.svelte';
  import DangerZone from '$lib/components/auth/DangerZone.svelte';
  import {
    KANBAN_COLUMNS,
    isBeadStatus,
    isTransitionAllowed,
    type BeadStatus
  } from '$lib/kanban';
  import { listBeads, transitionBead } from '$lib/api/beads';
  import type { Bead } from '$lib/types/bead';

  // Work kanban (hq-fe-view.5). 5 columns mirror gt-web `BeadStatus`. Drag
  // applies an optimistic intent (move locally first) → POST transition;
  // on 4xx the bead snaps back and the error surfaces inline. Close uses
  // DangerZone with the bead id as the typed-name.

  type Columns = Record<BeadStatus, Bead[]>;

  function emptyColumns(): Columns {
    return {
      pending: [],
      dispatched: [],
      working: [],
      done: [],
      failed: []
    };
  }

  let cols = $state<Columns>(emptyColumns());
  let loading = $state(true);
  let lastError = $state<string | null>(null);
  let closing = $state<Bead | null>(null);

  const FLIP_MS = 160;

  async function refresh() {
    loading = true;
    lastError = null;
    try {
      const results = await Promise.all(
        KANBAN_COLUMNS.map(async (s) => [s, await listBeads(s)] as const)
      );
      const next = emptyColumns();
      for (const [s, beads] of results) next[s] = beads;
      cols = next;
    } catch (e) {
      lastError = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  onMount(refresh);

  function handleDnd(target: BeadStatus, e: CustomEvent<DndEvent<Bead>>) {
    cols = { ...cols, [target]: e.detail.items };
  }

  async function finalize(target: BeadStatus, e: CustomEvent<DndEvent<Bead>>) {
    const items = e.detail.items;
    cols = { ...cols, [target]: items };

    const info = e.detail.info as { trigger?: string; id?: string };
    if (info.trigger !== 'droppedIntoZone') return;
    const moved = items.find((b) => b.id === info.id);
    if (!moved) return;

    const from = moved.status;
    if (from === target) return;
    if (!isBeadStatus(from)) {
      lastError = `bead ${moved.id} has unknown status "${from}"`;
      await refresh();
      return;
    }
    if (!isTransitionAllowed(from, target)) {
      lastError = `${from} → ${target} not allowed`;
      await refresh();
      return;
    }
    try {
      const updated = await transitionBead(moved.id, target);
      cols = {
        ...cols,
        [target]: cols[target].map((b) => (b.id === updated.id ? updated : b))
      };
      lastError = null;
    } catch (err) {
      lastError = err instanceof Error ? err.message : String(err);
      await refresh();
    }
  }

  async function doClose(bead: Bead) {
    if (!isBeadStatus(bead.status) || !isTransitionAllowed(bead.status, 'done')) {
      lastError = `cannot close from ${bead.status}`;
      return;
    }
    try {
      await transitionBead(bead.id, 'done');
      await refresh();
    } catch (err) {
      lastError = err instanceof Error ? err.message : String(err);
    }
  }
</script>

<section class="flex h-full min-h-0 flex-col gap-3 p-4">
  <header class="flex items-baseline justify-between">
    <div>
      <h2 class="font-sketch text-3xl" style="color: var(--accent)">Work</h2>
      <p class="mt-0.5 font-mono text-[11px]" style="color: var(--ink-faint)">
        kanban · 5 cols · drag to transition · close = typed-name (hq-fe-view.5)
      </p>
    </div>
    <button
      type="button"
      class="rounded border px-3 py-1.5 font-mono text-xs"
      style="border-color: var(--border); color: var(--ink-soft); background: var(--paper-2)"
      onclick={refresh}
      disabled={loading}
    >
      {loading ? '…' : 'reload'}
    </button>
  </header>

  {#if lastError}
    <div
      class="rounded border px-3 py-1.5 font-mono text-xs"
      style="border-color: var(--bad); color: var(--bad); background: var(--bad-soft)"
      role="alert"
    >
      {lastError}
    </div>
  {/if}

  <div class="grid min-h-0 flex-1 grid-cols-5 gap-2">
    {#each KANBAN_COLUMNS as status (status)}
      <section
        class="flex min-h-0 flex-col rounded border"
        style="border-color: var(--border); background: var(--paper-2)"
      >
        <header
          class="flex items-baseline justify-between border-b px-3 py-2 font-mono text-[11px] uppercase tracking-wider"
          style="border-color: var(--border-soft); color: var(--ink-soft)"
        >
          <span>{status}</span>
          <span style="color: var(--ink-faint)">{cols[status].length}</span>
        </header>

        <div
          class="flex flex-1 flex-col gap-1.5 overflow-auto p-2"
          use:dndzone={{
            items: cols[status],
            flipDurationMs: FLIP_MS,
            type: 'kanban',
            dropTargetStyle: { outline: '1px dashed var(--accent)', outlineOffset: '-2px' }
          }}
          onconsider={(e) => handleDnd(status, e)}
          onfinalize={(e) => finalize(status, e)}
        >
          {#each cols[status] as bead (bead.id)}
            <article
              animate:flip={{ duration: FLIP_MS }}
              class="rounded border p-2"
              style="border-color: var(--border-soft); background: var(--paper)"
            >
              <div class="flex items-baseline justify-between gap-2">
                <code class="font-mono text-[10px]" style="color: var(--accent)">{bead.id}</code>
                <span
                  class="rounded px-1 font-mono text-[9px]"
                  style:color={bead.priority === 0
                    ? 'var(--bad)'
                    : bead.priority === 1
                      ? 'var(--warn)'
                      : 'var(--ink-faint)'}
                >
                  P{bead.priority}
                </span>
              </div>
              <div class="mt-1 font-body text-xs" style="color: var(--ink)">{bead.title}</div>
              {#if bead.assignee}
                <div class="mt-1 font-mono text-[10px]" style="color: var(--ink-faint)">
                  {bead.assignee}
                </div>
              {/if}
              <Guard scope="bead.close">
                <button
                  type="button"
                  class="mt-2 rounded border px-2 py-0.5 font-mono text-[10px]"
                  style="border-color: var(--bad); color: var(--bad); background: var(--bad-soft)"
                  onclick={() => (closing = bead)}
                >
                  Close…
                </button>
              </Guard>
            </article>
          {/each}
          {#if cols[status].length === 0}
            <div
              class="rounded border border-dashed p-3 text-center font-mono text-[10px]"
              style="border-color: var(--border-soft); color: var(--ink-faint)"
            >
              empty
            </div>
          {/if}
        </div>
      </section>
    {/each}
  </div>

  <DangerZone
    open={closing !== null}
    name={closing?.id ?? ''}
    title="Close bead"
    actionLabel="Close"
    description="Closing transitions the bead to `done`. Type the id to confirm."
    onclose={() => (closing = null)}
    onfire={async () => {
      if (closing) await doClose(closing);
    }}
  />
</section>
