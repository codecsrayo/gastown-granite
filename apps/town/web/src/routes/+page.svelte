<script lang="ts">
  // Placeholder hero: smoke that scaffold builds + Tailwind + theme vars resolve.
  // Real layout lands in hq-fe-view.1; route swap to /activity in hq-fe-view.3.
</script>

<main
  class="mx-auto flex min-h-screen max-w-2xl flex-col justify-center gap-6 p-12"
  style="color: var(--ink)"
>
  <h1 class="font-sketch text-5xl" style="color: var(--accent)">Gas Town</h1>

  <p class="font-body text-lg" style="color: var(--ink-soft)">
    Dashboard scaffold · hq-fe-build.1 · ready to host hq-fe-view.* features.
  </p>

  <ul class="font-mono text-sm" style="color: var(--ink-soft)">
    <li>· Svelte 5 (runes) + SvelteKit · adapter-static SPA</li>
    <li>· Tailwind + CSS vars (data-theme=dark canonical)</li>
    <li>· Vite proxy /api → gt-api :8787</li>
    <li>
      · Spec:
      <a class="underline" style="color: var(--accent)" href="/api/whoami">/api/whoami</a>
      (needs bearer · 401 until login route ships)
    </li>
  </ul>

  <p class="font-body text-xs" style="color: var(--ink-faint)">
    Next: <code>hq-fe-build.2..8</code> (api/sse/stores/types/proxy/tests/CI), then
    <code>hq-fe-view.1</code> (layout) + <code>hq-fe-view.3</code> (Activity hero).
  </p>
</main>
