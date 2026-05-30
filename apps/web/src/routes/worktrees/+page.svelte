<script lang="ts">
  // hq-fe-view.14 — SCM-like "Branches" panel. Lists every worktree under the town root
  // (gt-web /api/worktrees, hq-fe-api-r.8) with branch, HEAD sha, divergence vs. main, and
  // the dirty file list. Polls every 2s so an agent's commit/dirty-file change shows up
  // without a manual reload — same cadence VSCode's SCM view uses (cheap GET on local git).
  //
  // hq-fe-view.15 — cross-link with /api/beads?status=working: the bead-id parsed from the
  // claim branch is joined against the live bead snapshot so each row surfaces the bead
  // title + assignee (the agent actively on that worktree). Beads fetch is fire-and-forget
  // — failures leave `beadsById` empty and the badge falls back to the id-only render.
  //
  // Layout is intentionally bare: full Shell/Sidebar/Topbar arrive with hq-fe-view.1 and
  // wrap this route via +layout.svelte once that bead lands.

  import { onDestroy, onMount } from 'svelte';
  import { fetchWorktrees } from '$lib/api/worktrees';
  import { fetchIssues } from '$lib/api/issues';
  import type { Worktree } from '$lib/types/worktree';
  import type { Issue } from '$lib/types/issue';
  import { beadIdFromBranch } from '$lib/claim-branch';

  let { data } = $props<{
    data: { initial: Worktree[]; issues: Issue[]; error: string | null };
  }>();

  // Local mutable mirror of the load() output. `$effect` re-seeds on navigation so a fresh
  // server-side fetch replaces the polled snapshot when the user re-enters the route.
  let rows = $state<Worktree[]>([]);
  let issues = $state<Issue[]>([]);
  let error = $state<string | null>(null);
  $effect(() => {
    rows = data.initial;
    issues = data.issues;
    error = data.error;
  });
  // Derived index for O(1) badge enrichment per row. Rebuilds whenever the polled `issues`
  // array swaps; Svelte 5 `$derived` keeps the map identity stable across re-renders.
  let issuesById = $derived(new Map(issues.map((i) => [i.id, i])));
  let expanded = $state<Record<string, boolean>>({});
  let timer: ReturnType<typeof setInterval> | undefined;

  async function refresh() {
    // Parallel + independent: issues failing must not drop the worktrees view. Same posture
    // as the +page.ts loader so the polled state machine matches the first-paint contract.
    const [wtResult, issuesResult] = await Promise.allSettled([
      fetchWorktrees(),
      fetchIssues('open,working'),
    ]);
    if (wtResult.status === 'fulfilled') {
      rows = wtResult.value;
      error = null;
    } else {
      error = wtResult.reason instanceof Error ? wtResult.reason.message : String(wtResult.reason);
    }
    if (issuesResult.status === 'fulfilled') issues = issuesResult.value;
  }

  onMount(() => {
    timer = setInterval(refresh, 2000);
  });
  onDestroy(() => {
    if (timer) clearInterval(timer);
  });

  function toggle(path: string) {
    expanded[path] = !expanded[path];
  }

  function shortSha(sha: string): string {
    return sha.slice(0, 8);
  }

  // Map porcelain v2 xy -> single-letter glyph + Tailwind color. Same letters VSCode renders
  // in its SCM view so the badge is immediately readable; unknown codes fall back to the raw
  // xy string to preserve information rather than swallow it.
  function badge(xy: string): { label: string; color: string } {
    if (xy === '??') return { label: 'U', color: 'text-emerald-400' };
    if (xy.startsWith('A')) return { label: 'A', color: 'text-emerald-400' };
    if (xy.startsWith('D') || xy.endsWith('D')) return { label: 'D', color: 'text-rose-400' };
    if (xy.startsWith('R')) return { label: 'R', color: 'text-sky-400' };
    if (xy.startsWith('U') || xy.endsWith('U')) return { label: '!', color: 'text-amber-400' };
    if (xy.includes('M')) return { label: 'M', color: 'text-sky-400' };
    return { label: xy.trim() || '?', color: 'text-zinc-400' };
  }
</script>

<svelte:head>
  <title>Worktrees · Gas Town</title>
</svelte:head>

<main class="mx-auto max-w-5xl p-8 font-mono text-sm" style="color: var(--ink)">
  <header class="mb-6 flex items-baseline justify-between">
    <h1 class="font-sketch text-3xl" style="color: var(--accent)">Worktrees</h1>
    <span class="text-xs" style="color: var(--ink-faint)">
      polling every 2s · {rows.length} repo{rows.length === 1 ? '' : 's'}
    </span>
  </header>

  {#if error}
    <p class="mb-4 rounded border border-rose-500/40 bg-rose-500/10 p-3 text-rose-300">
      {error}
    </p>
  {/if}

  {#if rows.length === 0 && !error}
    <p style="color: var(--ink-faint)">
      Empty. Set <code>GT_TOWN_ROOT</code> on gt-web to enumerate worktrees.
    </p>
  {/if}

  <ul class="divide-y divide-white/5">
    {#each rows as wt (wt.path)}
      {@const open = expanded[wt.path] ?? false}
      {@const beadId = beadIdFromBranch(wt.branch)}
      {@const liveBead = beadId ? (issuesById.get(beadId) ?? null) : null}
      <li class="py-3">
        <button
          type="button"
          class="flex w-full items-center justify-between gap-4 text-left"
          onclick={() => toggle(wt.path)}
        >
          <span class="flex min-w-0 items-center gap-3">
            <span
              class="inline-block w-12 text-center text-xs font-semibold uppercase"
              style="color: var(--ink-faint)"
            >
              {wt.is_main ? 'main' : 'wt'}
            </span>
            <span class="truncate" style="color: var(--ink)">
              {wt.branch ?? '(detached)'}
            </span>
            {#if beadId}
              <span
                class="shrink-0 rounded border px-1.5 py-0.5 font-mono text-[10px] uppercase"
                style="color: var(--accent); border-color: var(--accent)"
                title={liveBead?.title ?? 'bead id parsed from claim/ branch convention'}
              >
                {beadId}
              </span>
            {/if}
            {#if liveBead?.title}
              <span class="truncate text-xs" style="color: var(--ink-soft)">
                {liveBead.title}
              </span>
            {/if}
            {#if liveBead?.assignee}
              <span
                class="shrink-0 rounded bg-white/5 px-1.5 py-0.5 text-[10px]"
                style="color: var(--ink-soft)"
                title="bead assignee — agent on this worktree"
              >
                @{liveBead.assignee}
              </span>
            {/if}
            <span class="truncate text-xs" style="color: var(--ink-faint)">
              {wt.path}
            </span>
          </span>
          <span class="flex shrink-0 items-center gap-3 text-xs">
            <span title="HEAD sha" style="color: var(--ink-soft)">{shortSha(wt.head)}</span>
            <span title="behind / ahead" style="color: var(--ink-soft)">
              ↓{wt.behind} ↑{wt.ahead}
            </span>
            <span
              class="inline-block min-w-[2.5rem] text-center"
              title="dirty files"
              style="color: {wt.dirty.length === 0 ? 'var(--ink-faint)' : 'var(--accent)'}"
            >
              {wt.dirty.length}
            </span>
            <span aria-hidden="true" style="color: var(--ink-faint)">
              {open ? '▾' : '▸'}
            </span>
          </span>
        </button>

        {#if open && wt.dirty.length > 0}
          <ul class="mt-2 ml-16 space-y-1 text-xs">
            {#each wt.dirty as f (f.path)}
              {@const b = badge(f.xy)}
              <li class="flex items-center gap-3">
                <span class="w-4 text-center font-semibold {b.color}">{b.label}</span>
                <span class="truncate" style="color: var(--ink-soft)">{f.path}</span>
                <span style="color: var(--ink-faint)">{f.xy}</span>
              </li>
            {/each}
          </ul>
        {/if}
      </li>
    {/each}
  </ul>
</main>
