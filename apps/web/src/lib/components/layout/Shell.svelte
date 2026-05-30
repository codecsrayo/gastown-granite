<script lang="ts">
  import Sidebar from './Sidebar.svelte';
  import Topbar from './Topbar.svelte';
  import Dock from './Dock.svelte';

  let { children } = $props();
</script>

<div class="flex h-screen w-screen overflow-hidden">
  <Sidebar />
  <div class="flex min-w-0 flex-1 flex-col">
    <Topbar />
    <main
      class="min-h-0 flex-1 overflow-auto"
      style="background: var(--paper)"
    >
      {@render children()}
    </main>
    <Dock />
  </div>
</div>
