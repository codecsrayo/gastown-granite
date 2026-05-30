<script lang="ts">
  import { goto } from '$app/navigation';
  import { auth } from '$lib/stores/auth.svelte';
  import { clearBearer } from '$lib/bearer';
  import ProfileBadge from './ProfileBadge.svelte';

  // Topbar profile dropdown: whoami summary + read-only toggle + logout.
  // Wired through `auth` (lib/stores/auth.svelte) so toggling read-only is
  // immediately reflected by every Guard/DangerButton on the page.

  let open = $state(false);
  let panel: HTMLDivElement | undefined = $state();
  let badge: HTMLDivElement | undefined = $state();

  function close() {
    open = false;
  }
  function toggle() {
    open = !open;
  }

  function onWindowClick(ev: MouseEvent) {
    if (!open) return;
    const t = ev.target as Node;
    if (panel?.contains(t) || badge?.contains(t)) return;
    close();
  }
  function onKey(ev: KeyboardEvent) {
    if (open && ev.key === 'Escape') close();
  }

  $effect(() => {
    if (typeof window === 'undefined') return;
    window.addEventListener('click', onWindowClick);
    window.addEventListener('keydown', onKey);
    return () => {
      window.removeEventListener('click', onWindowClick);
      window.removeEventListener('keydown', onKey);
    };
  });

  function logout() {
    clearBearer();
    auth.reset();
    close();
    goto('/login');
  }
</script>

<div class="relative" bind:this={badge}>
  <ProfileBadge onclick={toggle} expanded={open} />

  {#if open}
    <div
      bind:this={panel}
      role="menu"
      class="absolute right-0 z-40 mt-1 w-64 rounded border p-3 shadow-lg"
      style="border-color: var(--border); background: var(--paper); box-shadow: 0 6px 16px var(--shadow)"
    >
      <header class="mb-3">
        <div class="font-body text-sm" style="color: var(--ink)">
          {auth.actor ?? (auth.mode === 'dev' ? 'guest · dev' : 'unauthenticated')}
        </div>
        <div class="mt-0.5 font-mono text-[10px]" style="color: var(--ink-faint)">
          mode={auth.mode} · roles=[{auth.roles.join(',') || '—'}] · scopes={auth.scopes.size}
        </div>
      </header>

      <label
        class="flex cursor-pointer items-center justify-between rounded px-2 py-1.5 font-mono text-xs"
        style="color: var(--ink-soft)"
      >
        <span>Read-only mode</span>
        <input
          type="checkbox"
          class="h-3.5 w-3.5"
          checked={auth.readOnly}
          onchange={(ev) => auth.setReadOnly(ev.currentTarget.checked)}
        />
      </label>
      <p class="mb-2 px-2 font-mono text-[10px]" style="color: var(--ink-faint)">
        Hides every destructive control (forces *.read scopes only).
      </p>

      <div class="my-2 border-t" style="border-color: var(--border-soft)"></div>

      <button
        type="button"
        role="menuitem"
        class="block w-full rounded px-2 py-1.5 text-left font-mono text-xs"
        style="color: var(--bad); background: transparent"
        onclick={logout}
      >
        Logout
      </button>
    </div>
  {/if}
</div>
