# `apps/web/` — Gas Town dashboard SPA

SvelteKit 5 + Tailwind + adapter-static. Talks to the Rust API
(`gt-web` + `gt-mcp`). Replaces the retired Go dashboard
(`internal/web/`).

**Read first:** [`../docs/README.md`](../docs/README.md) — rules,
canonical decisions, links to architecture / API / features docs.

## Quick start

Prereq: Node 22+, pnpm 9+ (use `corepack enable pnpm` if missing).
The compose stack (`gt-api` on 8787) must be reachable for live data.

```sh
cd apps/web
pnpm install
pnpm dev               # http://localhost:5173 · proxy /api → :8787
pnpm build             # static SPA in build/
pnpm preview           # serve build/ locally
pnpm check             # svelte-check strict
pnpm lint              # prettier + eslint
pnpm test              # vitest (unit/logic)
pnpm test:e2e          # playwright (flows)
```

Override the API target with `VITE_GT_API_URL=https://gastown.example.com pnpm dev`.

## Stack (canonical · see `../docs/frontend-architecture.md`)

| Layer | Choice |
|---|---|
| Framework | SvelteKit + Svelte 5 (runes) |
| Adapter | `@sveltejs/adapter-static` (SPA · gt-api serves the build) |
| Routing | sub-paths (`/activity`, `/work`, `/crew/:role`) — no SSR |
| Styles | Tailwind + CSS vars (dark canonical via `[data-theme]`) |
| State | runes singletons in `lib/stores/*.svelte.ts` + SSE fan-out + optimistic |
| Auth | bearer JWT in `Authorization` header — no CSRF |
| Drag-drop | `svelte-dnd-action` (kanban Work) |
| Terminal | `@xterm/xterm` lazy (only when dock opens · post hq-fe-term spike) |
| Tests | Vitest (unit) + Playwright (e2e) |

## Layout (will fill in as beads close)

```
src/
├── app.html · app.css · app.d.ts
├── lib/
│   ├── api/         hq-fe-build.2
│   ├── sse/         hq-fe-build.3
│   ├── stores/      hq-fe-build.4
│   ├── types/       hq-fe-build.5
│   ├── components/  hq-fe-view.12 (Guard · DangerButton · DangerZone) + layout
│   └── features/    hq-fe-view.3..11 (per-domain)
└── routes/
    ├── +layout.svelte / +layout.ts
    ├── +page.svelte                      ← placeholder hero (this bead)
    ├── activity/ work/ sessions/ …       ← hq-fe-view.*
    └── login/                            ← hq-fe-view.2
```

## Don't

- Don't `fetch` directly from a component — use `lib/api/*`.
- Don't pass bearer via URL query — header only.
- Don't validate scopes only client-side — backend always enforces too.
- Don't open multiple `EventSource` — one singleton multiplexed.
- Don't poll for updates — use SSE; if a kind is missing, open a bead.
- Don't port `internal/web/` 1:1 — build against the Rust contract real,
  not the retired Go API.

Full anti-patterns list in [`../docs/frontend-architecture.md`](../docs/frontend-architecture.md#anti-patrones-no-hacer).
