<script lang="ts">
  import { onDestroy } from 'svelte';
  import {
    createDangerMachine,
    DEFAULT_ARM_MS,
    type DangerState
  } from '$lib/danger-button';

  // 1-step armable destructive button. Click once → armed (label switches
  // to "Confirm" + timer countdown). Click again within the window → fires
  // the action. Auto-disarms after ARM_MS so a stale arm can't fire later.

  interface Props {
    label?: string;
    armedLabel?: string;
    firingLabel?: string;
    armMs?: number;
    disabled?: boolean;
    onfire: () => void | Promise<void>;
    children?: import('svelte').Snippet;
  }

  let {
    label = 'Delete',
    armedLabel = 'Confirm',
    firingLabel = '…',
    armMs = DEFAULT_ARM_MS,
    disabled = false,
    onfire,
    children
  }: Props = $props();

  const m = createDangerMachine();
  let state = $state<DangerState>('idle');
  let timer: ReturnType<typeof setTimeout> | null = null;

  function clearTimer() {
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
  }

  function disarm() {
    clearTimer();
    state = m.cancel();
  }

  async function click() {
    if (disabled || state === 'firing') return;
    if (state === 'idle') {
      state = m.arm(performance.now());
      clearTimer();
      timer = setTimeout(() => {
        const next = m.expireIfStale(performance.now(), armMs);
        state = next;
        timer = null;
      }, armMs);
      return;
    }
    // armed → fire
    clearTimer();
    state = m.fire();
    try {
      await onfire();
    } finally {
      state = m.settle();
    }
  }

  onDestroy(clearTimer);

  let display = $derived(
    state === 'firing' ? firingLabel : state === 'armed' ? armedLabel : label
  );
</script>

<button
  type="button"
  class="rounded border px-3 py-1.5 font-mono text-xs transition-colors disabled:opacity-50"
  style:border-color={state === 'armed' ? 'var(--bad)' : 'var(--border)'}
  style:background={state === 'armed' ? 'var(--bad-soft)' : 'var(--paper-2)'}
  style:color={state === 'idle' ? 'var(--bad)' : 'var(--ink)'}
  aria-pressed={state === 'armed'}
  aria-busy={state === 'firing'}
  data-state={state}
  {disabled}
  onclick={click}
  onblur={state === 'armed' ? disarm : undefined}
>
  {#if children}{@render children()}{:else}{display}{/if}
</button>
