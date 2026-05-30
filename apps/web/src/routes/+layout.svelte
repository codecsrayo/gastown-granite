<script lang="ts">
  import '../app.css';
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import Shell from '$lib/components/layout/Shell.svelte';
  import { theme } from '$lib/stores/theme.svelte';
  import { setOn401 } from '$lib/api/client';
  import { fetchWhoami } from '$lib/api/whoami';
  import { clearBearer } from '$lib/bearer';
  import { auth } from '$lib/stores/auth.svelte';

  let { children } = $props();

  async function hydrateAuth() {
    try {
      const w = await fetchWhoami({ skip401Hook: true });
      auth.hydrate({ actor: w.actor, roles: w.roles, scopes: w.scopes });
    } catch {
      // /api/whoami unreachable — leave auth in dev mode so the UI still renders.
    }
  }

  onMount(() => {
    theme.hydrate();
    // Any 401 from `/api/*` clears the bearer + bounces to /login so the
    // user can paste a fresh token without the dashboard sitting in a
    // perma-unauthenticated state.
    setOn401(() => {
      clearBearer();
      auth.reset();
      goto('/login');
    });
    hydrateAuth();
    return () => setOn401(null);
  });
</script>

<Shell>
  {@render children()}
</Shell>
