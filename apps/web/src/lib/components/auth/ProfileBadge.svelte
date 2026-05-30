<script lang="ts">
  import { auth } from '$lib/stores/auth.svelte';

  // Minimal actor + primary-role chip. Lives in the Topbar; clicking it
  // opens ProfileMenu. Falls back to a neutral "guest" label in dev mode
  // (before whoami hydrates).

  interface Props {
    onclick?: () => void;
    expanded?: boolean;
  }
  let { onclick, expanded = false }: Props = $props();

  let label = $derived(auth.actor ?? (auth.mode === 'dev' ? 'guest · dev' : 'unauthenticated'));
  let role = $derived(auth.roles[0] ?? null);
</script>

<button
  type="button"
  class="flex items-center gap-2 rounded border px-2 py-1 font-mono text-xs"
  style="border-color: var(--border); color: var(--ink-soft); background: var(--paper-2)"
  aria-haspopup="menu"
  aria-expanded={expanded}
  {onclick}
>
  <span style="color: var(--ink)">{label}</span>
  {#if role}
    <span class="rounded px-1 text-[10px]" style="background: var(--accent-soft); color: var(--accent)">
      {role}
    </span>
  {/if}
  {#if auth.readOnly}
    <span class="rounded px-1 text-[10px]" style="background: var(--warn-soft); color: var(--warn)">
      RO
    </span>
  {/if}
</button>
