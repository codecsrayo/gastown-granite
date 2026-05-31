<script lang="ts">
  // Tab strip across the dock. One chip per open terminal; clicking focuses,
  // the `×` closes. Empty state lives in `TermPrompt.svelte` — this strip is
  // hidden when `terminals.ids` is empty so the dock keeps its compact look.

  import { terminals } from '$lib/stores/terminals.svelte';
</script>

{#if terminals.ids.length > 0}
  <div
    class="flex shrink-0 items-center gap-1 border-b px-2 py-1 font-mono text-[10px]"
    style="border-color: var(--border-soft); background: var(--paper-2)"
  >
    {#each terminals.ids as id (id)}
      {@const active = terminals.active === id}
      <span
        class="inline-flex items-center gap-1 rounded border px-1.5 py-0.5"
        style="border-color: {active ? 'var(--accent)' : 'var(--border)'};
               background: {active ? 'var(--paper)' : 'transparent'};
               color: {active ? 'var(--ink)' : 'var(--ink-soft)'}"
      >
        <button
          type="button"
          class="font-mono text-[10px]"
          onclick={() => terminals.focus(id)}
        >
          {id}
        </button>
        <button
          type="button"
          class="font-mono text-[10px]"
          style="color: var(--ink-faint)"
          aria-label="Close terminal {id}"
          onclick={() => terminals.close(id)}
        >
          ×
        </button>
      </span>
    {/each}
  </div>
{/if}
