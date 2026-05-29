# Arquitectura del frontend SvelteKit

Decisiones de stack, estructura de proyecto y patrones núcleo. Acompaña al
plan de migración ([frontend-migration-sveltekit.md](frontend-migration-sveltekit.md))
y al contrato API ([frontend-api-surface.md](frontend-api-surface.md)).

> **Para agentes:** este documento manda sobre la elección de librerías, la
> organización de carpetas y los patrones de estado. Si algo del código
> diverge de aquí, abre bead — no cambies arquitectura por cuenta propia.

## Stack canónico (no re-litigar sin acuerdo)

| Capa | Elección | Razón |
|---|---|---|
| Framework | **SvelteKit + Svelte 5 (runes)** | runes = reactividad fine-grained sin `writable()` ceremonia |
| Adapter | `@sveltejs/adapter-static` | SPA pura; `gt-api` sirve el build, no Node |
| Routing | sub-paths (`/activity`, `/work`, `/crew/:role`) | bookmarkable + back/forward + code-split por route |
| Estilos | **Tailwind + CSS vars** | dark/light vía `[data-theme]`; sin doble compile |
| State | **Svelte 5 runes** en `.svelte.ts` singletons | `$state` + `$derived` + `$effect` |
| Data fetch | `fetch` + bearer + SSE invalidation | sin TanStack Query (overkill por ahora) |
| Auth | bearer JWT en localStorage | sin CSRF; bearer en header `Authorization` |
| SSE | `EventSource` único global + fan-out por `type` | mismo `EventRecord` que el log |
| Drag-drop | `svelte-dnd-action` | kanban Work; ~5kb |
| Terminal | `@xterm/xterm` lazy-loaded | ~150kb solo si dock abierto |
| Build | Vite + `pnpm` lockfile | reproducible builds |
| Tests | Vitest (unit) + Playwright (e2e) | sin Jest/RTL |
| Lint | eslint + prettier + svelte-check estricto | CI gate |

**No incluir** (sin acuerdo): service worker / PWA, i18n runtime, IndexedDB
persistence, WebSocket genérico, GraphQL, Storybook.

## Layout del proyecto

```
apps/web/
├── package.json · pnpm-lock.yaml · svelte.config.js · vite.config.ts
├── tailwind.config.ts · tsconfig.json · postcss.config.cjs
├── src/
│   ├── app.html · app.css · app.d.ts
│   ├── lib/
│   │   ├── api/                          HTTP client
│   │   │   ├── client.ts                    fetch wrapper · bearer · idem-key · 401 redirect
│   │   │   ├── beads.ts                     list · create · patch · transition · close · comments
│   │   │   ├── sessions.ts                  list · spawn · kill · restart · interrupt
│   │   │   ├── quota.ts                     accounts · rotation · rotate · retire · login (pty)
│   │   │   ├── convoys.ts                   list · launch · pause · resume · fail-member
│   │   │   ├── merges.ts · patrols.ts · rigs.ts
│   │   │   ├── roles.ts                     catalog · skills (toggle) · scope
│   │   │   ├── whoami.ts                    actor + roles + scopes
│   │   │   └── errors.ts                    AppError → toast mapping
│   │   ├── sse/
│   │   │   ├── stream.ts                    singleton EventSource → /api/stream
│   │   │   ├── router.ts                    fan-out por event.type → store
│   │   │   └── kinds.ts                     enum string → store handler map
│   │   ├── stores/                          estado por dominio (.svelte.ts = runes outside component)
│   │   │   ├── auth.svelte.ts                  actor · roles · scopes · hasScope() · hasRole() · readOnly
│   │   │   ├── sessions.svelte.ts              Map<id, Session> + active derived
│   │   │   ├── beads.svelte.ts                 Map<id, Bead> + per-status derived (kanban)
│   │   │   ├── activity.svelte.ts              ring buffer (~500 EventRecord)
│   │   │   ├── quota.svelte.ts                 accounts + rotation_state
│   │   │   ├── convoys.svelte.ts · merge.svelte.ts · patrol.svelte.ts
│   │   │   ├── crew.svelte.ts                  roles + skills + scope matrix
│   │   │   ├── theme.svelte.ts                 dark/light + persist
│   │   │   └── toast.svelte.ts                 transient notifications
│   │   ├── types/
│   │   │   ├── dto.ts                       Session, Bead, Convoy, Merge, Quota, Skill, Role, …
│   │   │   ├── events.ts                    EventRecord + kind union (~50 kinds)
│   │   │   └── commands.ts                  shape inputs de write-routes
│   │   ├── components/
│   │   │   ├── ui/                          Button · Toggle · Pill · Meter · Hatch · Toast · Modal
│   │   │   ├── layout/                      Shell · Sidebar · Topbar · TabStrip · Dock
│   │   │   ├── theme/                       ThemeToggle
│   │   │   └── auth/                        Guard · DangerButton · DangerZone · ProfileBadge · ProfileMenu
│   │   ├── features/                        por dominio (componentes + helpers locales)
│   │   │   ├── sessions/                    SessionsTable · SessionRow · KillConfirm
│   │   │   ├── activity/                    ActivityFeed · EventRow · CategoryFilter
│   │   │   ├── work/                        KanbanBoard · Column · BeadCard · DragHandle
│   │   │   ├── convoys/ · merge/ · rigs/
│   │   │   ├── crew/                        RoleList · RolePanel · SkillToggle · ScopeMatrix
│   │   │   ├── quota/                       AccountCard · QuotaMeter · RotationChips · LoginFlow
│   │   │   └── terminal/                    XtermWrap · TermTabs · TermPrompt · AuthPaste
│   │   ├── utils/                           fmt · time · idem · debounce
│   │   └── config/                          API base URL · env reading
│   └── routes/
│       ├── +layout.svelte                   shell + sidebar + topbar + dock siempre montados
│       ├── +layout.ts                       token guard · hydrate auth + whoami · start SSE
│       ├── +page.svelte                     redirect → /activity
│       ├── activity/+page.svelte
│       ├── work/+page.svelte
│       ├── sessions/+page.svelte
│       ├── convoys/+page.svelte · merge/+page.svelte
│       ├── crew/+page.svelte · crew/[role]/+page.svelte
│       ├── rigs/+page.svelte
│       ├── login/+page.svelte               pega bearer · futuro: OAuth A
│       └── +error.svelte                    500/404
├── static/                                  favicon · pwa assets
└── tests/
    ├── unit/                                stores · sse router · reducers
    └── e2e/                                 login · kill session · kanban drag · skill toggle
```

## Patrones núcleo

### 1 — SSE singleton + fan-out

Una sola conexión `EventSource`. Cliente multiplexa por `event.type` →
handler del store correspondiente.

```ts
// lib/sse/stream.ts
let es: EventSource | null = null;
export function startStream(token: string) {
  if (es) return;
  es = new EventSource(`/api/stream`); // bearer ya hidratado en cookie helper o query
  es.onmessage = (ev) => router(JSON.parse(ev.data) as EventRecord);
  es.onerror   = () => { /* reconnect con Last-Event-ID viene gratis */ };
}

// lib/sse/router.ts
const handlers: Record<string, (r: EventRecord) => void> = {
  "agent.spawned":          (r) => sessions.applyCanonical(r),
  "agent.killed":           (r) => sessions.applyCanonical(r),
  "agent.heartbeat":        (r) => sessions.heartbeat(r),
  "scheduling.dispatched":  (r) => beads.applyCanonical(r),
  "merge.merged":           (r) => merge.applyCanonical(r),
  "quota.account_limited":  (r) => quota.applyCanonical(r),
  "quota.rotated":          (r) => quota.applyCanonical(r),
  "quota.login_url_ready":  (r) => quota.loginUrlReady(r),
  // … ~50 kinds
};
export function router(rec: EventRecord) {
  if (rec.type.startsWith("web.") || rec.type.startsWith("mcp.")) {
    activity.appendAudit(rec);   // categoría "audit" en Activity feed
    return;
  }
  handlers[rec.type]?.(rec);
  activity.append(rec);          // todos van al feed live
}
```

**Reconnect** automático con `Last-Event-ID` (`EventRecord.event_id` mirrored
al SSE `id:` por `gt-web`).

### 2 — Stores con runes (Svelte 5)

`.svelte.ts` permite runes fuera de componente. Singleton importable.

```ts
// lib/stores/sessions.svelte.ts
import type { Session, EventRecord } from "$lib/types";

class SessionStore {
  rows    = $state<Map<string, Session>>(new Map());
  pending = $state<Set<string>>(new Set());

  active = $derived(
    [...this.rows.values()].filter(s => s.state !== "done" && s.state !== "killed")
  );

  byRig = $derived.by(() => {
    const m = new Map<string, Session[]>();
    for (const s of this.active) {
      if (!m.has(s.rig)) m.set(s.rig, []);
      m.get(s.rig)!.push(s);
    }
    return m;
  });

  hydrate(snapshot: Session[]) {
    this.rows = new Map(snapshot.map(s => [s.id, s]));
  }

  applyIntent(id: string, patch: Partial<Session>) {
    const cur = this.rows.get(id); if (!cur) return;
    this.rows.set(id, { ...cur, ...patch });
    this.pending.add(id);
  }

  applyCanonical(rec: EventRecord) {
    // map EventRecord (kind + payload) → mutation; clear pending
  }

  revertIntent(id: string, prev: Session) {
    this.rows.set(id, prev); this.pending.delete(id);
  }
}
export const sessions = new SessionStore();
```

### 3 — Comando optimistic + reconcile

```ts
// features/sessions/kill.ts
export async function kill(id: string) {
  const prev = sessions.rows.get(id); if (!prev) return;
  sessions.applyIntent(id, { state: "killed" });
  try {
    await api.sessions.kill(id, idemKey(`kill:${id}`));
    // SSE agent.killed llegará y reemplazará el optimistic
  } catch (e) {
    sessions.revertIntent(id, prev);
    toast.error(`No se pudo matar ${id}: ${e.message}`);
  }
}
```

Mutation devuelve cuando el backend acepta; UI ya muestra estado pendiente;
SSE confirma o el timeout (5s) revierte.

### 4 — Routing = tab

```
/                ← redirect a /activity
/activity        ← default, hero canon (imagen)
/work            ← kanban
/sessions
/convoys · /merge
/crew  · /crew/:role
/rigs
/login
```

`+layout.svelte` raíz monta shell completo + dock + sidebar Quota. `<slot />`
= canvas. Tab strip son `<a>` con `href` (no `<button>` JS) → bookmark + back
gratis.

### 5 — Auth guard + hydrate

```ts
// routes/+layout.ts
import { redirect } from "@sveltejs/kit";

export const load = async ({ url }) => {
  const token = localStorage.getItem("gt-token");
  if (!token && url.pathname !== "/login") throw redirect(307, "/login");

  if (token) {
    api.client.setToken(token);
    sse.startStream(token);

    const me = await api.whoami.get();
    auth.hydrate(me);                       // { actor, roles, scopes }

    // Snapshots iniciales en paralelo (no bloquean route)
    Promise.all([
      api.sessions.list().then(sessions.hydrate),
      api.beads.list().then(beads.hydrate),
      api.quota.accounts().then(quota.hydrate),
    ]);
  }
  return {};
};
```

### 6 — RBAC en UI (Guard component)

```svelte
<!-- lib/components/auth/Guard.svelte -->
<script lang="ts">
  import { auth } from "$lib/stores/auth.svelte";
  let { scope, children, fallback } = $props<{
    scope: string;
    children: import("svelte").Snippet;
    fallback?: import("svelte").Snippet;
  }>();
  let allowed = $derived(auth.hasScope(scope));
</script>

{#if allowed}
  {@render children()}
{:else if fallback}
  {@render fallback()}
{/if}
```

Uso típico:

```svelte
<Guard scope="session.kill">
  <DangerButton onclick={() => kill(session.id)}>Kill</DangerButton>
</Guard>
```

**Regla:** destructivas críticas se ocultan (no se desactivan greyed) →
reduce ruido visual. Read-only de campos editables sí se muestra greyed
(más informativo).

### 7 — Tema dark/light

```ts
// lib/stores/theme.svelte.ts
class Theme {
  current = $state<"dark" | "light">(
    (localStorage.getItem("gt-theme") as any) ?? "dark"
  );
  toggle() {
    this.current = this.current === "dark" ? "light" : "dark";
    localStorage.setItem("gt-theme", this.current);
    document.documentElement.setAttribute("data-theme", this.current);
  }
}
export const theme = new Theme();
```

Tailwind config detecta `data-theme="dark"` selector. CSS vars cambian. Sin
re-render.

### 8 — Kanban drag-drop

`svelte-dnd-action` por columna. On drop:

```ts
async function onDrop(beadId: string, from: Status, to: Status) {
  beads.applyIntent(beadId, { status: to });
  try { await api.beads.transition(beadId, from, to, idemKey()); }
  catch { beads.revertIntent(beadId); toast.error("…"); }
}
```

State-machine backend rechaza ilegales → 409 → revert. Sin lógica de
transición en cliente (solo backend valida).

### 9 — Terminal lazy

```svelte
<!-- features/terminal/XtermWrap.svelte -->
<script lang="ts">
  import { onMount } from "svelte";
  let mounted = $state(false);
  onMount(async () => {
    const { Terminal }  = await import("@xterm/xterm");
    const { FitAddon }  = await import("@xterm/addon-fit");
    // wire up al transport elegido (post hq-fe-term.0 spike)
    mounted = true;
  });
</script>
```

Dock siempre en DOM; xterm se carga sólo si el dock se abre. Transport TBD
hasta el spike `hq-fe-term.0`.

## Decisiones explícitas y por qué

| Decisión | Razón |
|---|---|
| **Runes en `.svelte.ts`** | Stores singleton sin `writable()` ceremonia; `$derived` reemplaza selectors manuales |
| **Sub-paths por tab** (no query string) | Bookmarkable + back/forward + code-split por route |
| **Una conexión SSE global** | Backend broadcast best-effort; multiplexar en cliente = más simple |
| **Optimistic + SSE reconcile** | Latencia percibida ~0; canonical siempre vence |
| **No TanStack Query** | Cache redundante con stores; SSE = invalidation. Añadir si crece cross-tab persist |
| **No SSR** | gt-api no es Node; SSR requiere runtime extra |
| **Types manuales primero** | `frontend-api-surface.md` es chico; cuando crezca, generar de `JsonSchema` que ya emite gt-mcp |
| **Tailwind + CSS vars** | dark/light sin doble compile; tema runtime |
| **pnpm + lockfile commit** | Reproducible; sin sorpresas de minor bumps |
| **eslint estricto + svelte-check** | Catch TS errors before runtime; CI gate |
| **Vitest + Playwright** | Vitest = stores/logic; Playwright = flujos críticos UI |
| **Layout persistente** | Sidebar + Dock no remontan al cambiar tab → SSE no se interrumpe |

## Dependencias mínimas

```json
{
  "dependencies": {
    "@xterm/xterm": "^5",
    "@xterm/addon-fit": "^0.10",
    "svelte-dnd-action": "^0.9"
  },
  "devDependencies": {
    "@sveltejs/adapter-static": "^3",
    "@sveltejs/kit": "^2",
    "svelte": "^5",
    "vite": "^5",
    "typescript": "^5",
    "tailwindcss": "^3",
    "vitest": "^2",
    "@playwright/test": "^1",
    "eslint": "^9",
    "prettier": "^3",
    "prettier-plugin-svelte": "^3"
  }
}
```

Total runtime: ~50kb gzip (Svelte 5 + SvelteKit shell) + xterm lazy (~150kb
solo si dock abierto) + svelte-dnd-action (~5kb).

## Cómo arrancar (post-scaffold)

```bash
cd apps/web
pnpm install
pnpm dev                      # Vite proxy /api → http://localhost:8787

# en otra consola: levantar la API
cd ../../..
task hot-deploy               # rebuild + restart compose stack

# tests
pnpm test                     # vitest
pnpm test:e2e                 # playwright contra dev server

# build producción
pnpm build                    # genera apps/web/build/
# gt-api sirve ese build (hq-fe-cut.1)
```

## Cómo extender (recipes para agentes)

### Añadir una vista nueva

1. Crear `src/routes/<tab>/+page.svelte`.
2. Si necesita datos no expuestos, abre bead `hq-fe-api-r.*` antes de
   empezar — NO inventes endpoints.
3. Crea store correspondiente si no existe: `src/lib/stores/<dom>.svelte.ts`.
4. Si emite eventos nuevos, registra handler en `lib/sse/router.ts`.
5. Componentes de la feature van en `src/lib/features/<dom>/`.
6. Si el botón muta estado, envuélvelo en `<Guard scope="…">` siempre.
7. Tab strip: añade `<a href="/<tab>">` en `components/layout/TabStrip.svelte`.

### Añadir una acción destructiva nueva

1. Define el comando backend primero (`hq-fe-api-w.*`); no UI sin endpoint.
2. Define el scope en `roles.toml` y `mcp-scope.toml`.
3. Frontend: wrap en `<Guard scope="X">` + `<DangerButton>` (1-step armable)
   o `<DangerZone name="…">` (typed-name modal) según gravedad.
4. Aplica optimistic intent → POST → reconcile o revert.
5. Verifica que `web.invoked` lleve `command` + `target` (hq-fe-rbac.5).

### Añadir un evento SSE nuevo

1. Definir el kind en backend (snake_case dot-separated: `domain.action`).
2. Añadir variante a `lib/types/events.ts` (union string literal).
3. Registrar handler en `lib/sse/router.ts`.
4. Si afecta UI, asegúrate que el store correspondiente sabe procesarlo.

## Anti-patrones (no hacer)

- ❌ Llamar a `fetch` directo desde un componente — usar `lib/api/*`.
- ❌ Pasar el bearer por URL query — header `Authorization` only.
- ❌ Validar permisos solo en frontend — el backend siempre valida también.
- ❌ Múltiples `EventSource` — un solo singleton multiplexado.
- ❌ Polling para "actualizaciones" — usa SSE; si falta evento, abre bead.
- ❌ Estado de UI en localStorage (excepto token + tema) — los stores son la
  fuente de verdad; localStorage = persistencia opcional.
- ❌ Rutas dinámicas con datos sensibles en query — usa path params.
- ❌ Reescribir el frontend Go viejo en Svelte — construye contra contrato
  Rust real, ver [frontend-api-surface.md](frontend-api-surface.md).

## Cross-refs

- Plan: [frontend-migration-sveltekit.md](frontend-migration-sveltekit.md)
- API: [frontend-api-surface.md](frontend-api-surface.md)
- Features: [frontend-features.md](frontend-features.md)
- Wireframe: [Gas Town Redesign Wireframes.html](Gas%20Town%20Redesign%20Wireframes.html)
- Mockup hi-fi: [pagina.png](pagina.png)
- Backend design: [apps/api/docs/07-frontend.md](../../api/docs/07-frontend.md)
