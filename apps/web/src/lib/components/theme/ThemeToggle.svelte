<script lang="ts">
  import { theme } from '$lib/stores/theme.svelte';
</script>

<button
  type="button"
  class="rounded border px-2 py-1 font-mono text-xs"
  style="border-color: var(--border); color: var(--ink-soft); background: var(--paper-2)"
  aria-label="Toggle theme"
  aria-pressed={theme.current === 'light'}
  onclick={() => theme.toggle()}
>
  {theme.current === 'dark' ? 'dark' : 'light'}
</button>
