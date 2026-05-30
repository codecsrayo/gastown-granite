<script lang="ts">
  import { page } from '$app/stores';
  import ThemeToggle from '$lib/components/theme/ThemeToggle.svelte';

  function pageTitle(pathname: string): string {
    if (pathname === '/' || pathname === '') return 'Home';
    const seg = pathname.split('/').filter(Boolean)[0] ?? 'Home';
    return seg.charAt(0).toUpperCase() + seg.slice(1);
  }
</script>

<header
  class="flex h-12 shrink-0 items-center justify-between border-b px-4"
  style="border-color: var(--border); background: var(--paper)"
>
  <div class="flex items-baseline gap-3">
    <span class="font-mono text-xs uppercase tracking-wider" style="color: var(--ink-faint)">
      view
    </span>
    <h1 class="font-body text-base" style="color: var(--ink)">
      {pageTitle($page.url.pathname)}
    </h1>
  </div>

  <div class="flex items-center gap-2">
    <ThemeToggle />
    <span
      class="rounded border px-2 py-1 font-mono text-[10px]"
      style="border-color: var(--border-soft); color: var(--ink-faint)"
      title="Profile menu lands in hq-fe-view.13"
    >
      profile · .13
    </span>
  </div>
</header>
