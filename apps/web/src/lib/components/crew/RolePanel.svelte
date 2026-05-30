<script lang="ts">
  // hq-fe-view.8 — right detail pane for the role selected in `RoleList`.
  // Live sessions table is the only part with real data today (filtered slice
  // of `/api/sessions`); `SkillToggle` + `ScopeMatrix` are explicit placeholders
  // until hq-fe-skills.2/.3 ship the `/api/skills` + `/api/roles/:role/scope`
  // surfaces.

  import type { Session } from '$lib/types/session';
  import SkillToggle from './SkillToggle.svelte';
  import ScopeMatrix from './ScopeMatrix.svelte';

  type Props = { role: string; sessions: Session[] };
  let { role, sessions }: Props = $props();

  function stateColor(state: string): string {
    if (state === 'working') return 'var(--good)';
    if (state === 'killed') return 'var(--bad)';
    if (state === 'done') return 'var(--ink-faint)';
    return 'var(--ink)';
  }
</script>

<div class="flex flex-col gap-4">
  <header class="flex items-baseline justify-between">
    <h2 class="font-sketch text-2xl" style="color: var(--accent)">{role}</h2>
    <span class="font-mono text-xs" style="color: var(--ink-faint)">
      {sessions.length} live
    </span>
  </header>

  <section
    class="rounded border p-3"
    style="border-color: var(--border); background: var(--paper-2)"
  >
    <header class="mb-2 flex items-baseline justify-between">
      <h3 class="font-sketch text-base" style="color: var(--ink)">Sessions</h3>
      <span class="font-mono text-[10px]" style="color: var(--ink-faint)">
        polling /api/sessions
      </span>
    </header>

    {#if sessions.length === 0}
      <p class="font-mono text-xs" style="color: var(--ink-faint)">
        No live {role} sessions.
      </p>
    {:else}
      <table class="w-full border-separate font-mono text-xs" style="border-spacing: 0 2px">
        <thead class="text-[10px] uppercase" style="color: var(--ink-faint)">
          <tr>
            <th class="px-2 py-1 text-left">id</th>
            <th class="px-2 py-1 text-left">rig</th>
            <th class="px-2 py-1 text-left">crew</th>
            <th class="px-2 py-1 text-left">state</th>
          </tr>
        </thead>
        <tbody>
          {#each sessions as s (s.id)}
            <tr>
              <td class="px-2 py-1" style="color: var(--ink)">{s.id}</td>
              <td class="px-2 py-1" style="color: var(--ink-soft)">{s.rig}</td>
              <td class="px-2 py-1" style="color: var(--ink-soft)">{s.crew ?? '—'}</td>
              <td class="px-2 py-1" style="color: {stateColor(s.state)}">{s.state}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  </section>

  <SkillToggle {role} />
  <ScopeMatrix {role} />
</div>
