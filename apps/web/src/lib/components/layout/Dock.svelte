<script lang="ts">
  // Dock = persistent bottom strip; terminal mount lives in hq-fe-view.11
  // (blocked by hq-fe-term spike). Here we ship the collapsed shell only:
  // a header bar with a toggle, no body, no xterm lazy-load yet.

  let open = $state(false);
</script>

<footer
  class="shrink-0 border-t"
  style="border-color: var(--border); background: var(--paper-2)"
>
  <div class="flex h-8 items-center justify-between px-3">
    <div class="flex items-center gap-2 font-mono text-[11px]" style="color: var(--ink-soft)">
      <span style="color: var(--ink-faint)">dock</span>
      <span style="color: var(--ink-faint)">·</span>
      <span title="Terminal mount in hq-fe-view.11">term ·.11</span>
    </div>
    <button
      type="button"
      class="rounded border px-2 py-0.5 font-mono text-[10px]"
      style="border-color: var(--border); color: var(--ink-soft); background: var(--paper)"
      aria-expanded={open}
      onclick={() => (open = !open)}
    >
      {open ? 'collapse' : 'expand'}
    </button>
  </div>

  {#if open}
    <div
      class="h-32 border-t px-3 py-2 font-mono text-xs"
      style="border-color: var(--border-soft); color: var(--ink-faint)"
    >
      Terminal pane placeholder. xterm + tmux attach ship in hq-fe-view.11
      (gated on the hq-fe-term spike).
    </div>
  {/if}
</footer>
