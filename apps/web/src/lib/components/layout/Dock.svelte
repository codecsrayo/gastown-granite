<script lang="ts">
  // Dock = persistent bottom strip (hq-fe-view.11).
  //
  // Collapsed: header bar only (open-tab count, expand button).
  // Expanded: TermTabs strip + active terminal (or TermPrompt when no tab).
  //
  // `@xterm/xterm` (~150kb gz) loads lazily via dynamic import the first time the
  // dock expands. Subsequent expansions reuse the loaded chunk.

  import type { Component } from 'svelte';
  import TermTabs from '../terminal/TermTabs.svelte';
  import TermPrompt from '../terminal/TermPrompt.svelte';
  import { terminals } from '$lib/stores/terminals.svelte';

  let open = $state(false);
  // `Component` holds the lazily-imported `XtermWrap.svelte`. `null` until the
  // dynamic import lands; the body shows a "loading…" placeholder in between.
  let XtermWrap = $state<Component<{ sessionId: string }> | null>(null);
  let loading = $state(false);

  async function loadXterm(): Promise<void> {
    if (XtermWrap || loading) return;
    loading = true;
    try {
      const mod = await import('../terminal/XtermWrap.svelte');
      XtermWrap = mod.default;
    } finally {
      loading = false;
    }
  }

  function toggle(): void {
    open = !open;
    if (open) void loadXterm();
  }
</script>

<footer
  class="shrink-0 border-t"
  style="border-color: var(--border); background: var(--paper-2)"
>
  <div class="flex h-8 items-center justify-between px-3">
    <div class="flex items-center gap-2 font-mono text-[11px]" style="color: var(--ink-soft)">
      <span style="color: var(--ink-faint)">dock</span>
      <span style="color: var(--ink-faint)">·</span>
      <span>term</span>
      {#if terminals.ids.length > 0}
        <span style="color: var(--ink-faint)">·</span>
        <span style="color: var(--ink)">{terminals.ids.length} open</span>
      {/if}
    </div>
    <button
      type="button"
      class="rounded border px-2 py-0.5 font-mono text-[10px]"
      style="border-color: var(--border); color: var(--ink-soft); background: var(--paper)"
      aria-expanded={open}
      onclick={toggle}
    >
      {open ? 'collapse' : 'expand'}
    </button>
  </div>

  {#if open}
    <div class="flex h-72 flex-col border-t" style="border-color: var(--border-soft)">
      <TermTabs />
      <div class="min-h-0 flex-1">
        {#if loading}
          <div class="px-3 py-2 font-mono text-[11px]" style="color: var(--ink-faint)">
            Loading xterm…
          </div>
        {:else if !XtermWrap}
          <TermPrompt />
        {:else if terminals.active === null}
          <TermPrompt />
        {:else}
          <!--
            `{#key}` forces a remount when the active tab changes — each session
            owns its own xterm + WebSocket, so we cannot swap `sessionId` props
            on a live instance without resetting state.
          -->
          {#key terminals.active}
            <XtermWrap sessionId={terminals.active} />
          {/key}
        {/if}
      </div>
    </div>
  {/if}
</footer>
