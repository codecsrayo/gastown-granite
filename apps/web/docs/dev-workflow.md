# Dev workflow

End-to-end loop for running the SvelteKit dashboard against `gt-web` while
iterating on the API + the UI. Closes hq-fe-build.6.

> Read first: [`README.md`](README.md) (rules) + [`frontend-architecture.md`](frontend-architecture.md) (stack).

---

## Prereqs

| Tool | Version | Why |
|---|---|---|
| Node | 22+ | SvelteKit 2, native `crypto.randomUUID`, `globalThis.fetch`. |
| pnpm | 9+ | matches the lockfile + `engines` shape in CI. `corepack enable pnpm` if missing. |
| gt-web | latest `cargo build --release -p gt-web` from `apps/api/` | the dashboard's only backend dep. |

The compose stack already exposes `gt-api` on the host-side `gastown-gt-api`
container, but vite's dev proxy targets `127.0.0.1:8787` by default; if you run
`gt-web` host-side (the typical inner loop, see below) the proxy hits that
binary directly.

---

## Daily loop

```sh
cd apps/web
pnpm install            # only after a lockfile bump
pnpm dev                # http://localhost:5173 · proxy /api → :8787
```

Vite's HMR rebuilds the SPA on save. Routes live under `src/routes/<name>/+page.svelte`;
new ones become reachable at the same path the moment the file exists.

### What the proxy forwards

`vite.config.ts` routes the four prefixes the dashboard talks to:

| Path | Target | Notes |
|---|---|---|
| `/api/*`     | `gt-api:8787` | All REST + SSE (`/api/stream`, `/api/worktrees/stream`). `ws: true` so the dev proxy stays alive for long-lived `EventSource` connections. |
| `/metrics`   | `gt-api:8787` | Prometheus scrape — open during dev for local Grafana. |
| `/health`    | `gt-api:8787` | Liveness probe. |
| `/readyz`    | `gt-api:8787` | Readiness probe — fails fast when Dolt is unreachable. |

Override the target with `VITE_GT_API_URL=https://gastown.codecsrayo.com pnpm dev`
when the local stack is down + you want to point at staging.

---

## Pre-flight commands

These match the CI workflow at `.github/workflows/web-ci.yml` (hq-fe-build.8).
Run them before pushing so the gate fails locally instead of in the cloud.

```sh
pnpm check              # svelte-check strict (svelte-kit sync first)
pnpm exec eslint .      # lint substantive issues
pnpm test               # vitest (unit + logic)
pnpm build              # vite build · adapter-static · writes build/
```

`pnpm lint` is the legacy combo `prettier --check . && eslint .` but the
prettier half currently breaks on `getVisitorKeys is not a function`
(prettier 3.8 + prettier-plugin-svelte 3.5 compat). Until the upstream fix
lands, run `pnpm exec eslint .` directly — CI does the same.

`pnpm format` rewrites the TS/JS/CSS slice (svelte files still fail to
format because of the same plugin bug; tolerate the trailing error, the
useful files have already been written).

---

## Running against real data

The default `pnpm dev` boots **in-memory** mode — the bus + the sessions
registry are empty so most snapshot endpoints serve `[]`. Two ways to hit
real state:

### Option A · `gt-web` host-side against Dolt (recommended for FE iteration)

Dolt's container port is on a custom bridge (`gastown_default`); the host
can reach it directly at `172.19.0.3:3307` (find with
`docker inspect gastown-dolt --format '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}'`).

```sh
GT_WEB_AUTH=disabled \
GT_TOWN_ROOT=/home/nixos/gastown \
GT_WEB_BIND=127.0.0.1:8788 \
GT_EVENT_LOG=/tmp/gt-web-host.events.jsonl \
GT_DOLT_URL=mysql://gastown@172.19.0.3:3307/hq \
setsid /home/nixos/gastown/apps/api/target/release/gt-web \
  > /tmp/gt-web-host.log 2>&1 < /dev/null & disown

# Point vite at the host binary instead of the compose container.
VITE_GT_API_URL=http://127.0.0.1:8788 pnpm dev
```

`setsid ... & disown` is important: a bare `&` leaves the binary attached
to the shell session, and a follow-up `pkill -f "target/release/gt-web"`
kills both that **and** any other gt-web (compose container restart races
with this kind of cleanup).

### Option B · stack container (`gastown-gt-api`)

Already exposes `/api/*` through Traefik at `gastown.codecsrayo.com`; the
container has no `GT_TOWN_ROOT` mount yet, so `/api/worktrees` serves `[]`
(see hq-fe-api-r.8 deploy-edge note in `frontend-migration-sveltekit.md`).
Useful when you just want to exercise the SSE bus + dolt-backed reads
without spawning a host binary.

---

## Authoring routes

1. New view → `mkdir -p src/routes/<name>` + `touch +page.svelte`.
2. Use `StubView` (`$lib/components/layout/StubView.svelte`) while the bead
   is open so the route is reachable + clearly marks itself unfinished.
3. Loaders go in `+page.ts` (SvelteKit SPA loader; `ssr=false` is set
   globally so they run client-side only).
4. Reach the API through `$lib/api/client.ts` (hq-fe-build.2) — never call
   `fetch('/api/...')` directly; the wrapper attaches bearer + idem-key.
5. Reach the SSE bus through `$lib/sse.ts` (hq-fe-build.3) —
   `subscribe('agent.*', fn)` returns its own unsubscribe so it composes
   with Svelte 5 `$effect`.

---

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| Dev proxy returns 502 on `/api/*` | `gt-web` not listening on the target | `task deploy-gt-api` (compose) or boot host-side per Option A. |
| `/api/worktrees` returns `[]` in compose | container has no `GT_TOWN_ROOT` | Run gt-web host-side (deploy-edge bead pending). |
| SSE silent for 30s+ | proxy buffering / WS upgrade dropped | reload the page (the singleton router auto-reconnects via the browser); confirm `ws: true` in `vite.config.ts` is still set. |
| `pnpm test` errors `Cannot find package 'jsdom'` | dev dep missing | `pnpm add -D jsdom` (vite config pins `environment: 'jsdom'`). |
| `pnpm lint` errors `getVisitorKeys is not a function` | prettier 3.8 + plugin 3.5 incompat | use `pnpm exec eslint .`; CI does the same. |
| `[vite:import-analysis] Failed to resolve import "<pkg>"` | `package.json` + lockfile updated but `node_modules/` stale (agent forgot `pnpm install`) | `cd apps/web && pnpm install`, then `pnpm exec vite build` to confirm. Agents must always run install + build/check after touching deps — see project `.claude/CLAUDE.md`. |
