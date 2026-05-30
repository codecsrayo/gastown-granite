<script lang="ts">
  import '../app.css';
  import { onMount } from 'svelte';
  import Shell from '$lib/components/layout/Shell.svelte';
  import { theme } from '$lib/stores/theme.svelte';

  let { children } = $props();

  onMount(() => {
    theme.hydrate();
  });
</script>

<Shell>
  {@render children()}
</Shell>
