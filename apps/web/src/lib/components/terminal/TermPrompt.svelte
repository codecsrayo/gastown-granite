<script lang="ts">
  // Empty-state for the dock body when no tabs are open. Lists running sessions
  // (from the live sessions store) so the operator can click one to attach.

  import { sessions } from '$lib/stores/sessions.svelte';
  import { terminals } from '$lib/stores/terminals.svelte';

  let candidates = $derived(sessions.rows.filter((r) => r.state !== 'closed'));
</script>

<div class="flex h-full min-h-0 flex-col items-stretch gap-2 px-3 py-2">
  <div class="font-mono text-[11px]" style="color: var(--ink-soft)">
    No terminal attached. Pick a session:
  </div>
  {#if candidates.length === 0}
    <div class="font-mono text-[11px]" style="color: var(--ink-faint)">
      No live sessions in registry.
    </div>
  {:else}
    <div class="flex flex-wrap gap-1.5">
      {#each candidates as s (s.id)}
        <button
          type="button"
          class="rounded border px-2 py-0.5 font-mono text-[10px]"
          style="border-color: var(--border); color: var(--ink-soft); background: var(--paper)"
          onclick={() => terminals.open(s.id)}
        >
          {s.id}
          <span style="color: var(--ink-faint)">· {s.state}</span>
        </button>
      {/each}
    </div>
  {/if}
</div>
