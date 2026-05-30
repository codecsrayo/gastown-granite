<script lang="ts">
  import { page } from '$app/stores';
  import QuotaPanel from '$lib/components/quota/QuotaPanel.svelte';

  type NavItem = { href: string; label: string; hint?: string };

  // Nav matches frontend-architecture.md routes table. Order = canonical sidebar order.
  const items: NavItem[] = [
    { href: '/activity', label: 'Activity', hint: 'feed' },
    { href: '/sessions', label: 'Sessions', hint: 'tmux' },
    { href: '/work', label: 'Work', hint: 'kanban' },
    { href: '/worktrees', label: 'Worktrees', hint: 'scm' },
    { href: '/convoys', label: 'Convoys' },
    { href: '/merge', label: 'Merge Q' },
    { href: '/crew', label: 'Crew' },
    { href: '/rigs', label: 'Rigs' }
  ];

  function isActive(href: string, pathname: string): boolean {
    if (href === '/') return pathname === '/';
    return pathname === href || pathname.startsWith(href + '/');
  }
</script>

<aside
  class="flex w-56 shrink-0 flex-col overflow-y-auto border-r"
  style="border-color: var(--border); background: var(--paper)"
>
  <div class="px-4 pb-3 pt-5">
    <a
      href="/"
      class="font-sketch text-2xl"
      style="color: var(--accent)"
      aria-label="Gas Town home"
    >
      Gas Town
    </a>
  </div>

  <nav class="flex flex-col gap-px px-2" aria-label="Primary">
    {#each items as item (item.href)}
      {@const active = isActive(item.href, $page.url.pathname)}
      <a
        href={item.href}
        class="flex items-baseline justify-between rounded px-3 py-2 font-mono text-sm transition-colors"
        style:color={active ? 'var(--accent)' : 'var(--ink-soft)'}
        style:background={active ? 'var(--accent-soft)' : 'transparent'}
        aria-current={active ? 'page' : undefined}
      >
        <span>{item.label}</span>
        {#if item.hint}
          <span class="text-[10px]" style="color: var(--ink-faint)">{item.hint}</span>
        {/if}
      </a>
    {/each}
  </nav>

  <div class="mt-auto">
    <QuotaPanel />
  </div>
</aside>
