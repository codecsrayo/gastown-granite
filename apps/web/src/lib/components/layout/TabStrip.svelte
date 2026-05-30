<script lang="ts">
  // Generic sub-nav strip for views with multiple panels (Activity audit
  // filter, Crew per-role, etc). Stateless: consumer owns selection.
  type Tab = { id: string; label: string; hint?: string };

  interface Props {
    tabs: Tab[];
    current: string;
    onSelect?: (id: string) => void;
  }

  let { tabs, current, onSelect }: Props = $props();
</script>

<div
  class="flex items-stretch gap-px border-b"
  style="border-color: var(--border)"
  role="tablist"
>
  {#each tabs as tab (tab.id)}
    {@const active = tab.id === current}
    <button
      type="button"
      role="tab"
      aria-selected={active}
      class="flex items-baseline gap-2 border-b-2 px-3 py-2 font-mono text-xs transition-colors"
      style:color={active ? 'var(--ink)' : 'var(--ink-soft)'}
      style:border-color={active ? 'var(--accent)' : 'transparent'}
      onclick={() => onSelect?.(tab.id)}
    >
      <span>{tab.label}</span>
      {#if tab.hint}
        <span class="text-[10px]" style="color: var(--ink-faint)">{tab.hint}</span>
      {/if}
    </button>
  {/each}
</div>
