<script lang="ts">
  interface Props {
    title: string;
    bead: string;
    summary?: string;
  }
  let { title, bead, summary }: Props = $props();
</script>

<section class="mx-auto max-w-3xl p-8">
  <header class="mb-3">
    <h2 class="font-sketch text-3xl" style="color: var(--accent)">{title}</h2>
    <div class="mt-1 font-mono text-[11px]" style="color: var(--ink-faint)">
      tracked by {bead}
    </div>
  </header>

  {#if summary}
    <p class="font-body text-sm" style="color: var(--ink-soft)">{summary}</p>
  {/if}

  <div
    class="mt-6 rounded border border-dashed p-6 font-mono text-xs"
    style="border-color: var(--border-soft); color: var(--ink-faint)"
  >
    Stub route. Real content lands in {bead}; the layout shell (hq-fe-view.1)
    is in place so subsequent views drop straight into this canvas.
  </div>
</section>
