<script lang="ts">
  // hq-fe-view.8 — Crew tab. Canonical layout per frontend-features.md §8:
  // left rail `RoleList` over the six Gas Town roles, right pane `RolePanel`
  // showing live sessions + skills/scope slots. Skills + scope panels render
  // as explicit placeholders until hq-fe-skills.2/.3 ship `/api/roles` and
  // `/api/roles/:role/scope`; the live session roster is real and polls
  // `/api/sessions` every 3s (same cadence as `/sessions`).

  import { onDestroy, onMount } from 'svelte';
  import { fetchSessions } from '$lib/api/sessions';
  import type { Session } from '$lib/types/session';
  import RoleList from '$lib/components/crew/RoleList.svelte';
  import RolePanel from '$lib/components/crew/RolePanel.svelte';

  let { data } = $props<{ data: { initial: Session[]; error: string | null } }>();

  // Static catalog; the eventual `GET /api/roles` payload supersedes this list.
  // Order matches the canon table (mayor on top, polecat at the floor).
  const roles: string[] = ['mayor', 'sheriff', 'deacon', 'refinery', 'witness', 'polecat'];

  let rows = $state<Session[]>([]);
  let error = $state<string | null>(null);
  let selected = $state<string>(roles[0]);

  $effect(() => {
    rows = data.initial;
    error = data.error;
  });

  let counts = $derived.by(() => {
    const acc: Record<string, number> = {};
    for (const r of roles) acc[r] = 0;
    for (const s of rows) acc[s.role] = (acc[s.role] ?? 0) + 1;
    return acc;
  });

  let panelSessions = $derived(rows.filter((s) => s.role === selected));

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
</script>

<svelte:head>
  <title>Crew · Gas Town</title>
</svelte:head>

<section class="font-mono text-sm" style="color: var(--ink)">
  <header class="mb-6 flex flex-wrap items-baseline justify-between gap-3">
    <h1 class="font-sketch text-3xl" style="color: var(--accent)">Crew</h1>
    <span class="text-xs" style="color: var(--ink-faint)">
      polling every 3s · {rows.length} sessions
    </span>
  </header>

  {#if error}
    <p class="mb-4 rounded border border-rose-500/40 bg-rose-500/10 p-3 text-rose-300">
      {error}
    </p>
  {/if}

  <div class="grid grid-cols-[12rem_1fr] gap-6">
    <RoleList {roles} {counts} {selected} onselect={(r) => (selected = r)} />
    <RolePanel role={selected} sessions={panelSessions} />
  </div>
</section>
