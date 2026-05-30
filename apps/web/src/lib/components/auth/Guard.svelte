<script lang="ts">
  import { auth } from '$lib/stores/auth.svelte';

  // Destructive-aware permission gate.
  //
  // Per frontend-architecture.md §"Read-only mode": destructive controls
  // (e.g. Kill) HIDE on missing scope; read-only editables show greyed via
  // the `editable` mode, which renders a non-interactive snapshot.

  interface Props {
    scope?: string;
    role?: string;
    mode?: 'destructive' | 'editable';
    children?: import('svelte').Snippet;
    fallback?: import('svelte').Snippet;
  }

  let { scope, role, mode = 'destructive', children, fallback }: Props = $props();

  let allowed = $derived(
    (scope === undefined || auth.hasScope(scope)) &&
      (role === undefined || auth.hasRole(role))
  );
</script>

{#if allowed}
  {@render children?.()}
{:else if mode === 'editable'}
  <span aria-disabled="true" style="opacity: 0.5; pointer-events: none">
    {@render children?.()}
  </span>
{:else if fallback}
  {@render fallback()}
{/if}
