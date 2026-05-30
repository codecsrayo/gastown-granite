<script lang="ts">
  import { goto } from '$app/navigation';
  import { writeBearer } from '$lib/bearer';
  import { auth } from '$lib/stores/auth.svelte';

  // Pre-RBAC login flow: paste the bearer JWT, persist, and bounce home.
  // /api/whoami hydration lands with hq-fe-rbac.4; until then dev mode stays
  // permissive so even an empty bearer renders the dashboard.

  let token = $state('');
  let busy = $state(false);
  let error = $state<string | null>(null);

  async function submit(ev: SubmitEvent) {
    ev.preventDefault();
    const trimmed = token.trim();
    if (!trimmed) {
      error = 'Paste a bearer token first.';
      return;
    }
    busy = true;
    error = null;
    try {
      writeBearer(trimmed);
      // Best-effort: when hq-fe-rbac.4 ships /api/whoami this is where we
      // hydrate `auth`. For now we leave auth in dev mode so the dashboard
      // stays usable.
      await goto('/');
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  function dev() {
    // Sentinel value: the +layout.ts bearer guard only checks "any token",
    // and lib/api/* (hq-fe-build.2) treats this exact string as "do not
    // send an Authorization header". Lets a dev browse the SPA without a
    // real JWT while keeping the guard mandatory in production.
    writeBearer('dev');
    auth.reset();
    goto('/');
  }
</script>

<section class="mx-auto flex max-w-md flex-col gap-5 p-10">
  <header>
    <h2 class="font-sketch text-3xl" style="color: var(--accent)">Login</h2>
    <p class="mt-1 font-body text-sm" style="color: var(--ink-soft)">
      Paste a bearer token to authenticate against gt-api. Pre-RBAC
      (hq-fe-rbac.4) the token is stored verbatim and dev mode stays on.
    </p>
  </header>

  <form class="flex flex-col gap-3" onsubmit={submit}>
    <label class="font-mono text-xs" style="color: var(--ink-soft)">
      Bearer token
      <textarea
        class="mt-1 block h-24 w-full resize-none rounded border bg-transparent p-2 font-mono text-xs outline-none"
        style="border-color: var(--border); color: var(--ink)"
        spellcheck="false"
        autocomplete="off"
        placeholder="eyJhbGciOiJI…"
        bind:value={token}
        disabled={busy}
      ></textarea>
    </label>

    {#if error}
      <p class="font-mono text-xs" style="color: var(--bad)">{error}</p>
    {/if}

    <div class="flex items-center justify-between">
      <button
        type="button"
        class="rounded border px-3 py-1.5 font-mono text-xs"
        style="border-color: var(--border-soft); color: var(--ink-soft); background: var(--paper-2)"
        onclick={dev}
        disabled={busy}
      >
        Skip · dev mode
      </button>
      <button
        type="submit"
        class="rounded border px-3 py-1.5 font-mono text-xs"
        style="border-color: var(--accent); color: var(--accent); background: var(--accent-soft)"
        disabled={busy}
      >
        {busy ? '…' : 'Sign in'}
      </button>
    </div>
  </form>

  <p class="font-mono text-[10px]" style="color: var(--ink-faint)">
    Real OAuth (Claude account) deferred to hq-fe-auth; pty driver is the
    backup path documented in apps/web/docs/frontend-features.md §13.
  </p>
</section>
