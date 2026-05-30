<script lang="ts">
  import { page } from '$app/stores';
  import ThemeToggle from '$lib/components/theme/ThemeToggle.svelte';
  import ProfileMenu from '$lib/components/auth/ProfileMenu.svelte';

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
    <ProfileMenu />
  </div>
</header>
