<script lang="ts">
  // Typed-name confirmation modal for catastrophic actions (drop convoy,
  // close epic). User must type the exact `name` to enable the fire button.

  interface Props {
    open: boolean;
    name: string;
    title?: string;
    actionLabel?: string;
    description?: string;
    onclose: () => void;
    onfire: () => void | Promise<void>;
  }

  let {
    open,
    name,
    title,
    actionLabel = 'Delete',
    description,
    onclose,
    onfire
  }: Props = $props();

  let typed = $state('');
  let firing = $state(false);

  let match = $derived(typed === name);

  $effect(() => {
    if (!open) {
      typed = '';
      firing = false;
    }
  });

  async function fire() {
    if (!match || firing) return;
    firing = true;
    try {
      await onfire();
      onclose();
    } finally {
      firing = false;
    }
  }
</script>

{#if open}
  <div
    class="fixed inset-0 z-50 flex items-center justify-center p-4"
    style="background: rgba(0, 0, 0, 0.6)"
    role="dialog"
    aria-modal="true"
    aria-labelledby="danger-zone-title"
  >
    <div
      class="w-full max-w-md rounded border p-5"
      style="background: var(--paper); border-color: var(--bad)"
    >
      <header class="mb-3">
        <h2 id="danger-zone-title" class="font-body text-lg" style="color: var(--bad)">
          {title ?? `Confirm ${actionLabel}`}
        </h2>
        {#if description}
          <p class="mt-1 font-body text-sm" style="color: var(--ink-soft)">{description}</p>
        {/if}
      </header>

      <label class="block font-mono text-xs" style="color: var(--ink-soft)">
        Type
        <code class="rounded px-1" style="background: var(--bad-soft); color: var(--ink)">
          {name}
        </code>
        to confirm:
        <!-- svelte-ignore a11y_autofocus -->
        <input
          type="text"
          class="mt-2 block w-full rounded border bg-transparent px-2 py-1 font-mono text-sm outline-none"
          style:border-color={match ? 'var(--bad)' : 'var(--border)'}
          style:color="var(--ink)"
          autocomplete="off"
          spellcheck="false"
          bind:value={typed}
          autofocus
        />
      </label>

      <footer class="mt-4 flex justify-end gap-2">
        <button
          type="button"
          class="rounded border px-3 py-1.5 font-mono text-xs"
          style="border-color: var(--border); color: var(--ink-soft); background: var(--paper-2)"
          onclick={onclose}
        >
          Cancel
        </button>
        <button
          type="button"
          class="rounded border px-3 py-1.5 font-mono text-xs disabled:opacity-40"
          style:border-color="var(--bad)"
          style:background={match ? 'var(--bad)' : 'var(--bad-soft)'}
          style:color={match ? 'var(--paper)' : 'var(--bad)'}
          disabled={!match || firing}
          aria-busy={firing}
          onclick={fire}
        >
          {firing ? '…' : actionLabel}
        </button>
      </footer>
    </div>
  </div>
{/if}
