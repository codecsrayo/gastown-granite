<script lang="ts">
  // hq-fe-view.8 placeholder — real toggle ships with hq-fe-skills.3
  // (`POST /api/roles/:role/skills`). Until the gt-skills crate (hq-fe-skills.1)
  // exposes `GET /api/skills`, the panel renders a disabled stub so operators
  // see the slot without us inventing a fake skill list.

  type Props = { role: string };
  let { role }: Props = $props();
</script>

<section
  class="rounded border p-3 font-mono text-xs"
  style="border-color: var(--border); background: var(--paper-2)"
>
  <header class="mb-2 flex items-baseline justify-between">
    <h3 class="font-sketch text-base" style="color: var(--ink)">Skills</h3>
    <span class="text-[10px]" style="color: var(--ink-faint)">
      hq-fe-skills.2/.3 pending
    </span>
  </header>
  <p style="color: var(--ink-faint)">
    Skill catalog for <span style="color: var(--ink)">{role}</span> will load
    from <code>GET /api/skills</code> once the gt-skills crate lands. Toggle
    writes target <code>POST /api/roles/{role}/skills</code>.
  </p>
</section>
