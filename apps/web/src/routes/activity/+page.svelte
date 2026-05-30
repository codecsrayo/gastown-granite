<script module lang="ts">
  // Tiny Svelte action: when `deps` changes + autoScroll is on, pin the container to its
  // bottom. Keeps the feed live without forcing scroll if the operator scrolled up to
  // read history.
  export function scrollOnUpdate(
    node: HTMLElement,
    params: { autoScroll: boolean; deps: number }
  ) {
    let { autoScroll, deps: _deps } = params;
    void _deps; // referenced via `update` so Svelte re-runs the action
    function pin() {
      if (autoScroll) node.scrollTop = node.scrollHeight;
    }
    pin();
    return {
      update(next: { autoScroll: boolean; deps: number }) {
        autoScroll = next.autoScroll;
        _deps = next.deps;
        pin();
      }
    };
  }
</script>

<script lang="ts">
  // hq-fe-view.3 — Activity feed (canon hero per `pagina.png`). Subscribes to the SSE
  // bus via `lib/sse` (hq-fe-build.3), pushes every frame into the `activity` ring buffer
  // (hq-fe-build.4), and renders the buffer filtered by category + rig + free text.
  //
  // History (`/api/feed?since=…`, hq-fe-api-r.5) is still open, so the feed starts empty
  // on mount and only grows from live SSE. When r.5 ships, hydrate the store from the
  // load() loader so the first paint already carries the last N minutes.

  import { onDestroy, onMount } from 'svelte';
  import { subscribe, subscribeStatus, type SseStatus } from '$lib/sse';
  import { activity } from '$lib/stores/activity.svelte';
  import { CATEGORIES, categoryOf, type Category } from '$lib/event-category';
  import { relativeAge } from '$lib/relative-time';

  let categoryFilter = $state<Category | ''>('');
  let rigFilter = $state<string>('');
  let textFilter = $state<string>('');
  let autoScroll = $state<boolean>(true);
  let status = $state<SseStatus>('closed');

  let unsubFrames: (() => void) | undefined;
  let unsubStatus: (() => void) | undefined;

  onMount(() => {
    unsubFrames = subscribe('*', (rec) => activity.push(rec));
    unsubStatus = subscribeStatus((s) => (status = s));
  });
  onDestroy(() => {
    unsubFrames?.();
    unsubStatus?.();
  });

  // Each frame is enriched with its category once + reused in every render. The wire ts
  // is RFC3339 — convert to Unix seconds for the relative-age chip.
  let enriched = $derived(
    activity.events.map((e) => ({
      ...e,
      category: categoryOf(e.type),
      // `rig` lives on the payload for most domains (agent/work/quota); the audit slice
      // doesn't carry a rig so the column shows `—`.
      rig: (e.payload as { rig?: string } | null)?.rig ?? null,
      tsSecs: Math.floor(Date.parse(e.ts) / 1000)
    }))
  );

  let rigs = $derived(unique(enriched.map((e) => e.rig).filter((r): r is string => !!r)));

  let visible = $derived(
    enriched.filter((e) => {
      if (categoryFilter && e.category !== categoryFilter) return false;
      if (rigFilter && e.rig !== rigFilter) return false;
      if (textFilter) {
        const needle = textFilter.toLowerCase();
        const hay = (e.type + ' ' + JSON.stringify(e.payload)).toLowerCase();
        if (!hay.includes(needle)) return false;
      }
      return true;
    })
  );

  function unique(xs: string[]): string[] {
    return [...new Set(xs)].sort();
  }

  function categoryColor(c: Category): string {
    switch (c) {
      case 'agent':
        return 'var(--accent)';
      case 'work':
        return 'var(--good)';
      case 'quota':
        return 'var(--warn)';
      case 'audit':
        return 'var(--ink-faint)';
      default:
        return 'var(--ink-soft)';
    }
  }

  function statusColor(s: SseStatus): string {
    if (s === 'open') return 'var(--good)';
    if (s === 'error') return 'var(--bad)';
    if (s === 'connecting') return 'var(--warn)';
    return 'var(--ink-faint)';
  }
</script>

<svelte:head>
  <title>Activity · Gas Town</title>
</svelte:head>

<section class="font-mono text-sm" style="color: var(--ink)">
  <header class="mb-4 flex flex-wrap items-baseline justify-between gap-3">
    <h1 class="font-sketch text-3xl" style="color: var(--accent)">Activity</h1>
    <span class="flex items-center gap-3 text-xs" style="color: var(--ink-faint)">
      <span style="color: {statusColor(status)}">● {status}</span>
      <span>{visible.length} of {enriched.length}</span>
    </span>
  </header>

  <div class="mb-3 flex flex-wrap items-center gap-3 text-xs">
    <label class="flex items-center gap-2">
      <span style="color: var(--ink-faint)">cat</span>
      <select
        bind:value={categoryFilter}
        class="rounded border bg-transparent px-2 py-1"
        style="border-color: var(--border); color: var(--ink)"
      >
        <option value="">all</option>
        {#each CATEGORIES as c}
          <option value={c}>{c}</option>
        {/each}
      </select>
    </label>
    <label class="flex items-center gap-2">
      <span style="color: var(--ink-faint)">rig</span>
      <select
        bind:value={rigFilter}
        class="rounded border bg-transparent px-2 py-1"
        style="border-color: var(--border); color: var(--ink)"
      >
        <option value="">all</option>
        {#each rigs as r}
          <option value={r}>{r}</option>
        {/each}
      </select>
    </label>
    <label class="flex items-center gap-2">
      <span style="color: var(--ink-faint)">search</span>
      <input
        type="search"
        bind:value={textFilter}
        placeholder="kind / payload substring"
        class="rounded border bg-transparent px-2 py-1"
        style="border-color: var(--border); color: var(--ink); min-width: 18rem"
      />
    </label>
    <label class="ml-auto flex items-center gap-2">
      <input type="checkbox" bind:checked={autoScroll} />
      <span style="color: var(--ink-faint)">auto-scroll</span>
    </label>
  </div>

  {#if enriched.length === 0}
    <p style="color: var(--ink-faint)">
      Waiting for the SSE bus. Live frames will land here as the reactor emits them.
    </p>
  {/if}

  <ul
    class="divide-y divide-white/5 overflow-y-auto"
    style="max-height: 70vh"
    use:scrollOnUpdate={{ autoScroll, deps: visible.length }}
  >
    {#each visible as e (e.event_id)}
      <li class="flex items-center gap-3 py-1.5">
        <span
          class="w-12 shrink-0 text-[10px] uppercase tracking-wide"
          style="color: {categoryColor(e.category)}"
        >
          {e.category}
        </span>
        <span class="w-12 shrink-0 text-xs" style="color: var(--ink-faint)">
          {relativeAge(e.tsSecs)}
        </span>
        <span class="w-48 shrink-0 truncate" style="color: var(--ink)">{e.type}</span>
        <span class="w-24 shrink-0 truncate text-xs" style="color: var(--ink-soft)">
          {e.rig ?? '—'}
        </span>
        <span class="min-w-0 flex-1 truncate text-xs" style="color: var(--ink-faint)">
          {JSON.stringify(e.payload)}
        </span>
      </li>
    {/each}
  </ul>
</section>
