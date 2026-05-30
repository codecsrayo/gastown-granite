<script lang="ts">
  // Landing page. Real redirect to /activity ships with hq-fe-view.3
  // (when the Activity feed exists). For now: hub linking to live routes.

  type Card = { href: string; title: string; bead: string; status: 'live' | 'stub' };
  const cards: Card[] = [
    { href: '/worktrees', title: 'Worktrees', bead: 'hq-fe-view.14-16', status: 'live' },
    { href: '/activity', title: 'Activity', bead: 'hq-fe-view.3', status: 'stub' },
    { href: '/sessions', title: 'Sessions', bead: 'hq-fe-view.4', status: 'stub' },
    { href: '/work', title: 'Work', bead: 'hq-fe-view.5', status: 'stub' },
    { href: '/convoys', title: 'Convoys', bead: 'hq-fe-view.6', status: 'stub' },
    { href: '/merge', title: 'Merge Q', bead: 'hq-fe-view.7', status: 'stub' },
    { href: '/crew', title: 'Crew', bead: 'hq-fe-view.8', status: 'stub' },
    { href: '/rigs', title: 'Rigs', bead: 'hq-fe-view.9', status: 'stub' },
    { href: '/design', title: 'Design', bead: 'hq-fe-view.12', status: 'live' }
  ];
</script>

<section class="mx-auto max-w-4xl p-8">
  <header class="mb-6">
    <h2 class="font-sketch text-3xl" style="color: var(--accent)">Dashboard</h2>
    <p class="mt-1 font-body text-sm" style="color: var(--ink-soft)">
      Layout shell (hq-fe-view.1) · pick a view below or use the sidebar.
    </p>
  </header>

  <ul class="grid grid-cols-2 gap-3 sm:grid-cols-3">
    {#each cards as card (card.href)}
      <li>
        <a
          href={card.href}
          class="block rounded border p-3 transition-colors hover:border-current"
          style="border-color: var(--border); background: var(--paper-2)"
        >
          <div class="flex items-baseline justify-between">
            <span class="font-body text-base" style="color: var(--ink)">{card.title}</span>
            <span
              class="rounded px-1.5 py-0.5 font-mono text-[10px]"
              style:color={card.status === 'live' ? 'var(--accent)' : 'var(--ink-faint)'}
              style:background={card.status === 'live'
                ? 'var(--accent-soft)'
                : 'var(--paper)'}
            >
              {card.status}
            </span>
          </div>
          <div class="mt-1 font-mono text-[10px]" style="color: var(--ink-faint)">
            {card.bead}
          </div>
        </a>
      </li>
    {/each}
  </ul>
</section>
