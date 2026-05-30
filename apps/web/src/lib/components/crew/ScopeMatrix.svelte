<script lang="ts">
  // hq-fe-view.8 placeholder — real allow/deny matrix ships with hq-fe-skills.2
  // (`GET /api/roles/:role/scope`) once gt-skills exposes the role→scope binding.
  // Inlining a hand-rolled fallback would drift from the canonical scope table
  // (mcp-scope.toml), so we leave the slot empty and explicit.

  type Props = { role: string };
  let { role }: Props = $props();
</script>

<section
  class="rounded border p-3 font-mono text-xs"
  style="border-color: var(--border); background: var(--paper-2)"
>
  <header class="mb-2 flex items-baseline justify-between">
    <h3 class="font-sketch text-base" style="color: var(--ink)">MCP scope</h3>
    <span class="text-[10px]" style="color: var(--ink-faint)">
      hq-fe-skills.2 pending
    </span>
  </header>
  <p style="color: var(--ink-faint)">
    Allow/deny matrix for <span style="color: var(--ink)">{role}</span> reads
    from <code>GET /api/roles/{role}/scope</code>. Source of truth is
    <code>mcp-scope.toml</code>.
  </p>
</section>
