<script lang="ts">
  import Guard from '$lib/components/auth/Guard.svelte';
  import DangerButton from '$lib/components/auth/DangerButton.svelte';
  import DangerZone from '$lib/components/auth/DangerZone.svelte';
  import { auth } from '$lib/stores/auth.svelte';

  let killed = $state(0);
  let zoneOpen = $state(false);
  let dropped = $state(false);

  function fakeAsync<T>(value: T, ms = 600): Promise<T> {
    return new Promise((r) => setTimeout(() => r(value), ms));
  }

  async function kill() {
    await fakeAsync(null);
    killed += 1;
  }

  async function drop() {
    await fakeAsync(null);
    dropped = true;
  }

  function snapshot(): { mode: string; readOnly: boolean } {
    return { mode: auth.mode, readOnly: auth.readOnly };
  }
</script>

<section class="mx-auto max-w-3xl space-y-8 p-8 font-body">
  <header>
    <h2 class="font-sketch text-3xl" style="color: var(--accent)">Design · auth primitives</h2>
    <p class="mt-1 text-sm" style="color: var(--ink-soft)">
      Guard / DangerButton / DangerZone live playground (hq-fe-view.12).
    </p>
  </header>

  <article class="space-y-3">
    <h3 class="font-mono text-xs uppercase tracking-wider" style="color: var(--ink-faint)">
      Auth store
    </h3>
    <div
      class="rounded border p-3 font-mono text-xs"
      style="border-color: var(--border-soft); color: var(--ink-soft); background: var(--paper-2)"
    >
      mode={snapshot().mode} · readOnly={snapshot().readOnly} · actor={auth.actor ?? 'null'}
    </div>
    <div class="flex flex-wrap gap-2">
      <button
        type="button"
        class="rounded border px-3 py-1.5 font-mono text-xs"
        style="border-color: var(--border); color: var(--ink-soft); background: var(--paper-2)"
        onclick={() => auth.reset()}
      >
        reset (dev)
      </button>
      <button
        type="button"
        class="rounded border px-3 py-1.5 font-mono text-xs"
        style="border-color: var(--border); color: var(--ink-soft); background: var(--paper-2)"
        onclick={() =>
          auth.hydrate({
            actor: 'demo-operator',
            roles: ['operator'],
            scopes: ['session.read']
          })}
      >
        operator (read-only scopes)
      </button>
      <button
        type="button"
        class="rounded border px-3 py-1.5 font-mono text-xs"
        style="border-color: var(--border); color: var(--ink-soft); background: var(--paper-2)"
        onclick={() =>
          auth.hydrate({
            actor: 'demo-admin',
            roles: ['admin'],
            scopes: ['session.read', 'session.kill', 'convoy.fail']
          })}
      >
        admin (full)
      </button>
      <button
        type="button"
        class="rounded border px-3 py-1.5 font-mono text-xs"
        style="border-color: var(--border); color: var(--ink-soft); background: var(--paper-2)"
        onclick={() => auth.setReadOnly(!auth.readOnly)}
      >
        toggle readOnly
      </button>
    </div>
  </article>

  <article class="space-y-3">
    <h3 class="font-mono text-xs uppercase tracking-wider" style="color: var(--ink-faint)">
      Guard + DangerButton (Sessions kill pattern)
    </h3>
    <div class="flex items-center gap-3">
      <Guard scope="session.kill">
        <DangerButton label="Kill" armedLabel="Confirm kill" onfire={kill} />
      </Guard>
      <span class="font-mono text-xs" style="color: var(--ink-soft)">
        killed × {killed}
      </span>
    </div>
    <p class="font-mono text-[11px]" style="color: var(--ink-faint)">
      Hidden when missing `session.kill` (destructive default). Try the
      operator preset above to see it disappear.
    </p>
  </article>

  <article class="space-y-3">
    <h3 class="font-mono text-xs uppercase tracking-wider" style="color: var(--ink-faint)">
      DangerZone (Convoy e-stop pattern)
    </h3>
    <Guard scope="convoy.fail">
      <button
        type="button"
        class="rounded border px-3 py-1.5 font-mono text-xs"
        style="border-color: var(--bad); color: var(--bad); background: var(--bad-soft)"
        onclick={() => (zoneOpen = true)}
      >
        Drop convoy `demo-convoy`…
      </button>
    </Guard>
    {#if dropped}
      <div class="font-mono text-xs" style="color: var(--accent)">dropped ✓</div>
    {/if}

    <DangerZone
      open={zoneOpen}
      name="demo-convoy"
      title="Drop convoy"
      actionLabel="Drop"
      description="Type the convoy name to confirm this irreversible e-stop."
      onclose={() => (zoneOpen = false)}
      onfire={drop}
    />
  </article>

  <article class="space-y-3">
    <h3 class="font-mono text-xs uppercase tracking-wider" style="color: var(--ink-faint)">
      Guard mode="editable" (greyed read-only field)
    </h3>
    <Guard scope="bead.update" mode="editable">
      <input
        type="text"
        class="rounded border bg-transparent px-2 py-1 font-mono text-sm"
        style="border-color: var(--border); color: var(--ink)"
        placeholder="bead title…"
      />
    </Guard>
    <p class="font-mono text-[11px]" style="color: var(--ink-faint)">
      Editable fields stay visible but inert when scope missing (more
      informative than hiding).
    </p>
  </article>
</section>
