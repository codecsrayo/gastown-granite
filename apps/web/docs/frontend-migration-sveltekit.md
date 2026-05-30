# Plan completo · frontend SvelteKit (epic `hq-fe-svelte`)

Plan exhaustivo del rediseño del dashboard de Gas Town como SPA SvelteKit
sobre la API Rust existente (`apps/api/`, servida por `gt-web` + `gt-mcp`).
El frontend Go antiguo (`internal/web/`) **ya quedó retirado** del despliegue
(compose apunta `gastown.codecsrayo.com` a `gt-api` puerto 8787 desde
`c877758e`).

**No es migración con paridad.** La API Rust expone una superficie distinta y
más chica; el frontend se construye contra esa superficie, no replicando la
vieja UI.

Docs hermanos (lectura obligada antes de tocar nada):

- [frontend-api-surface.md](frontend-api-surface.md) — contrato API real + gaps tabulados.
- [frontend-architecture.md](frontend-architecture.md) — estructura SvelteKit, stores, SSE, optimistic UI.
- [frontend-features.md](frontend-features.md) — catálogo de features, cómo mapean al backend.
- [Gas Town Redesign Wireframes.html](Gas%20Town%20Redesign%20Wireframes.html) — V1 dark, 3 vistas (Activity · Work · Crew).
- [pagina.png](pagina.png) — render hi-fi (= V1 dark canónico).
- [apps/api/docs/07-frontend.md](../../api/docs/07-frontend.md) — diseño backend (gt-web).

---

> ## ⚠️ AVISO PARA AGENTES — leer antes de tocar el frontend
>
> Hay varios agentes trabajando en este repo. Este documento es la **fuente
> de verdad del alcance del frontend**. Reglas para no perder ni pisar
> trabajo:
>
> 1. **No portar 1:1 desde Go.** `internal/web/` está retirado del
>    despliegue. La API Rust no replica sus endpoints (`/api/run`,
>    `/api/mail/*`, `/api/issues/*`, etc.). Cada necesidad del frontend que
>    no aparezca en [frontend-api-surface.md](frontend-api-surface.md) es un
>    **gap explícito** que requiere bead — decidir caso a caso si se añade a
>    `gt-web`, se promueve desde `gt-mcp`, o se difiere.
> 2. **Reclama antes de trabajar.** Antes de empezar un bead, márcalo busy
>    (status=dispatched/working) y anótate en la tabla de estado abajo.
> 3. **Respeta dependencias entre epics.** El grafo está al final de este
>    doc. No te saltes el bus de comandos (`hq-fe-api-w.1`) — todo write-side
>    depende de él.
> 4. **Rama aparte → merge a main → borra la rama.** Nunca directo sobre
>    main (el town root revierte). Usa worktree.
> 5. **Decisiones tomadas (no re-litigar sin acuerdo):**
>    - framework = **SvelteKit + Svelte 5 (runes)**
>    - adapter = **static SPA** (gt-api sirve el build)
>    - tema canónico = **dark** (toggle light disponible)
>    - variante canónica = **V1** (tabs horizontales)
>    - auth = **bearer JWT** (no CSRF, no cookie)
>    - rutas SPA = sub-paths (`/activity`, `/work`, `/crew/:role`…) bookmarkables
>    - estado = stores runes + SSE fan-out + optimistic + reconcile
>    - una sola conexión SSE global multiplexada en cliente
>    - command-bus único en gt-root, mismo path para HTTP/MCP/CLI

---

## Estado global

**Estado: EN PROGRESO** (snapshot 2026-05-30).

| Epic | Descripción | Beads | Bloqueada por | Estado |
|---|---|---|---|---|
| **hq-fe-svelte** | Master · dashboard reconstruction | (todas abajo) | — | PLANEADO |
| **hq-fe-api-r** | Read-side gaps (snapshots por dominio) | 12 | — | EN PROGRESO · r.2-.12 CLOSED (11/12) · r.1 working |
| **hq-fe-api-w** | Write-side commands (HTTP routes) | 11 | hq-fe-api-w.1 (bus) | DONE · 11/11 CLOSED |
| **hq-fe-rbac** | RBAC · JWT · whoami · scopes | 5 | hq-fe-api-w.1 | DONE · 5/5 CLOSED |
| **hq-fe-auth** | Account auth (Claude `/login` pty driver) | 5 | hq-fe-api-w (idem) | PLANEADO |
| **hq-fe-skills** | Skills + Roles domain (nuevo) | 5 | hq-fe-rbac | PLANEADO |
| **hq-fe-term** | Terminal bridge (xterm + tmux) | 4 | spike `.0` | spike DECIDIDO ([doc](spike-hq-fe-term-0-transport.md)) → WS en gt-web · `.1`/`.2` desbloqueados |
| **hq-fe-build** | SvelteKit scaffold + tooling | 8 | — | EN PROGRESO · .1-.4 + .6 + .8 CLOSED (6/8) |
| **hq-fe-view** | Vistas + componentes (UI) | 19 | hq-fe-build + hq-fe-api-r | EN PROGRESO · view.1-.7/.12/.13/.14-.19 CLOSED (15/19) |
| **hq-fe-cut** | Cutover: gt-api sirve el build · borrar Go | 4 | hq-fe-view 80% | DONE · 3/4 closed (.1/.2/.4); .3 deferido a hq-oap5 |
| **hq-mcp-issues** | MCP `issues.*` CRUD (cerrar bypass docker exec) | 5 | hq-fe-api-w.1 | DONE · 5/5 closed |
| **hq-mcp-onboard** | MCP agent onboarding + discoverability (slogan-feedback gaps) | 10 | parcial hq-mcp-issues.2 + hq-fe-api-w.1 | DONE · claude-host-onboard |

Total ~90 beads. Tabla viva — actualiza al reclamar/cerrar.

---

## Fases (orden suelto · dentro de cada fase los epics corren en paralelo)

### Fase 0 — Fundación + inventario (semana 1)

**Objetivo:** unblock todo lo demás.

- `hq-fe-api-w.1` command-bus interno en gt-root (refactor; sin HTTP nuevo).
- `hq-fe-api-w.2` `Idempotency-Key` middleware en gt-web.
- `hq-fe-build.1` scaffold `apps/web/` (Svelte 5 + Tailwind + pnpm).
- `hq-fe-build.5` tipos TS desde [frontend-api-surface.md](frontend-api-surface.md).

**Gate:** `gt-root::commands::dispatch(cmd)` válido para todos los MCP tools actuales (refactor invisible); `apps/web/pnpm dev` arranca página vacía contra `/api`.

### Fase 1 — Read-side mínima (semana 2)

- `hq-fe-api-r.1..7` snapshots por dominio (quota, convoys, merges, feed, mayor/status, ?rig=).
- `hq-fe-rbac.1..4` JWT + roles.toml + middleware + `/api/whoami`.
- `hq-fe-build.2..4` lib/api + lib/sse + lib/stores base.

**Gate:** `curl -H "Authorization: Bearer …" /api/quota/accounts` devuelve snapshot completo; `/api/whoami` responde con roles + scopes; dashboard hidrata sidebar Quota desde snapshot real.

### Fase 2 — Write-side mínima (semana 3)

- `hq-fe-api-w.3..5` beads CRUD HTTP.
- `hq-fe-api-w.6..8` session kill / restart / interrupt.
- `hq-fe-view.1..2` layout raíz + login route + bearer guard.
- `hq-fe-view.10` Quota sidebar (componentes).
- `hq-fe-view.12` Guard / DangerButton / DangerZone components.

**Gate:** login con JWT → dashboard hidrata; click Kill en una sesión → SIGTERM real al polecat → SSE confirma → UI parchea.

### Fase 3 — Vistas core (semana 4)

- `hq-fe-view.3` Activity feed (canon de la imagen).
- `hq-fe-view.4` Sessions table.
- `hq-fe-view.5` Work kanban (drag-drop).
- `hq-fe-view.13` Profile menu topbar.

**Gate:** 4 vistas (Activity, Sessions, Work, layout completo) operativas contra `gt-api` real con SSE viva. Kanban drag → transición real.

### Fase 4 — Vistas avanzadas + skills (semana 5)

- `hq-fe-skills.1..5` dominio skills/roles + endpoints.
- `hq-fe-view.6..9` Convoys, Merge Q, Crew, Rigs.
- `hq-fe-auth.0..5` account login pty driver.
- `hq-fe-api-w.9..10` convoy pause/resume/fail · quota rotate/retire HTTP.

**Gate:** Crew tab permite togglear skills por rol; e-stop convoy con 2-step confirm funciona; Login button por cuenta arranca flujo pty real.

### Fase 5 — Terminal + cutover (semana 6)

- `hq-fe-term.0` spike decision (WebSocket vs MCP vs nuevo bin).
- `hq-fe-term.1..4` PTY adapter + WS endpoint + structured stream + auth.
- `hq-fe-view.11` dock terminal mount (xterm lazy).
- `hq-fe-cut.1..4` gt-api sirve build · traefik · borrar `internal/web/` · docs ops.

**Gate:** `https://gastown.codecsrayo.com` carga SPA nueva, dock attach a sesión tmux viva, Go web/ borrado del árbol.

---

## Tabla de beads (reclamar aquí)

> Al reclamar un bead: copia tu identidad de agente a la columna **Agente**,
> cambia **Estado** a `working`, abre una rama desde main, trabaja, mergea,
> cierra el bead. Si te bloquea otro bead, anótalo en **Notas**.

### Epic `hq-fe-svelte` (master)

| Bead | Título | Pri | Estado | Agente | Notas |
|---|---|---|---|---|---|
| hq-fe-svelte | Master epic — frontend reconstruction | P1 | open | — | tracks all children |

### Epic `hq-fe-api-r` — read-side gaps

| Bead | Título | Pri | Estado | Agente | Notas |
|---|---|---|---|---|---|
| hq-fe-api-r.1 | `GET /api/quota/accounts` snapshot completo | P1 | closed | claude-host | `QuotaAccountDto {id, state, tokens_used, tokens_cap, reset_at, sessions}`; status collapsed a `active`/`inactive`/`blocked`; window flattened (`consumed.ceil() as u64`); `sessions:[]` reservado hasta que `AccountRegistry` exponga índice; 2 cargo tests (empty + 3-status bucketing) sumados al `quota_http` suite |
| hq-fe-api-r.2 | `GET /api/quota/rotation` waiting_unlock + recent | P1 | closed | claude-host | snapshot 7fb78241 (commit en main); derivado de SSE quota.* + estado |
| hq-fe-api-r.3 | `GET /api/convoys` snapshot por estado | P2 | closed | claude-host | `ConvoyDto`+`ConvoyMemberDto`; `bus.orch().snapshot()`; `?state=` filter permissive; 10 cargo test; FE `lib/{types,api}/convoy*` |
| hq-fe-api-r.4 | `GET /api/merges` slots snapshot | P2 | closed | claude-host | `AppState<R,SQ,M>` add `M: MergeRepository` (14 fixtures patched); `MergeSlotDto {bead,branch,state}` flat strings; `routes::list_merges`; tests `merges_http` 2/2 (sorted seeded + empty). 12 fixtures gain `merges` field |
| hq-fe-api-r.5 | `GET /api/feed?since=&limit=` activity histórico | P2 | closed | claude-host | `gt-audit::since` reader sobre `events.jsonl` + `gt-web::routes::feed`; `since` strict-greater-than RFC3339, `limit` default 500/cap 2000; `AppState.event_log` cabled desde `root.log_path()`; tests `feed_http` 4/4 (tail/since/limit/unwired) |
| hq-fe-api-r.6 | `?rig=` filter en `/api/sessions` | P2 | closed | claude-host | `SessionsQuery.rig: Option<String>` AND con `role`; mismatch yields empty (view, not error); `fetchSessions({rig, role})` con back-compat string; tests sessions_role 2/2 cubren rig solo + combo + miss |
| hq-fe-api-r.7 | `GET /api/mayor/status` ATTACHED/DETACHED | P3 | closed | claude-host | `MayorStatusDto {attached, session_id?, rig?, state?}` derived del session registry (first row role=mayor); 3 cargo tests (attached/dogs-only/empty); `lib/{types,api}/mayor.ts` cliente. Heartbeat freshness deferred (agent relay aún no stamps per-role ts) |
| hq-fe-api-r.8 | `GET /api/worktrees` snapshot (live branch tracking) | P1 | closed | claude-host | shell `git worktree list` + `status --porcelain=v2` + `rev-list main...HEAD`; backs hq-fe-view.14 |
| hq-fe-api-r.9 | `GET /api/issues` snapshot (hq.issues mirror) | P1 | closed | claude-host | DoltIssues reader; filters status/assignee/external_ref/limit; mirror del `gt://issues` MCP; backs hq-fe-view.15 |
| hq-fe-api-r.10 | Extend `/api/worktrees` con HEAD subject + author | P2 | closed | claude-host | `git log -1 --format=%s%n%an` per wt; null gracefully; backs hq-fe-view.17 |
| hq-fe-api-r.11 | Extend `/api/worktrees` con HEAD time (Unix seconds) | P2 | closed | claude-host | mismo `git log -1` extendido a `%s%n%an%n%ct`; backs hq-fe-view.18 sort |
| hq-fe-api-r.12 | SSE `/api/worktrees/stream` — server poll + broadcast | P2 | closed | claude-host | 1 poller/proc; `broadcast::Sender<Vec<WorktreeDto>>`; `sse_from_json_receiver<T>` generic helper |

### Epic `hq-fe-api-w` — write-side commands

| Bead | Título | Pri | Estado | Agente | Notas |
|---|---|---|---|---|---|
| hq-fe-api-w.1 | command-bus interno en gt-root | P1 | closed | claude-host | dispatcher across 7 domains; unlock root write-side |
| hq-fe-api-w.2 | `Idempotency-Key` middleware en gt-web | P1 | closed | claude-host | TTL configurable via `GT_WEB_IDEMPOTENCY_TTL_SECS` |
| hq-fe-api-w.3 | `POST /api/beads` + `PATCH /api/beads/:id` | P1 | closed | claude-host | dispatch via CommandBus |
| hq-fe-api-w.4 | `POST /api/beads/:id/transition` state machine | P1 | closed | — | guard ilegales → 409 |
| hq-fe-api-w.5 | `POST /api/beads/:id/comments` | P2 | closed | claude-host | append-only notes column via `IssueCommenter` port (`DoltIssueCommenter` shares `Arc<DoltIssues>` con reader); canonical fragment `ts/author/body`; commit 9b522879 |
| hq-fe-api-w.6 | `DELETE /api/sessions/:id` kill via gt-polecat | P1 | closed | — | `PolecatControl::kill` port (`TmuxPolecatControl` over `gt_polecat::TmuxCli`); tmux kill BEFORE emit so fatal edge errors return 500 sin row half-closed; emits `AgentEvent::Killed` so projector + SSE see lifecycle close |
| hq-fe-api-w.7 | `POST /api/sessions/:id/restart` | P2 | closed | claude-host | `PolecatRespawner` port + `LifecyclePolecatRespawner` over `PolecatLifecycle`; reads dying session env (`GT_HOOK_BEAD`/`GT_CONVOY`) y respawn con misma crew; emits `Killed`+`Spawned` pair so projector flips in single tick |
| hq-fe-api-w.8 | `POST /api/sessions/:id/interrupt` (tmux ESC) | P2 | closed | — | shared `PolecatControl` port (`interrupt = send-keys Escape`); softer e-stop que no mata polecat; misma shape que kill (404 if missing, 500 if control unwired) |
| hq-fe-api-w.9 | `POST /api/convoys` + pause/resume/fail-member | P2 | closed | — | `POST /api/convoys` (LaunchConvoy via CommandBus, atomic launch + dispatch primer member) + `POST /api/convoys/:c/members/:m/fail` (`FailMember{convoy,member,reason}`); pause/resume deferred (domain no tiene Pause/Resume hoy) |
| hq-fe-api-w.10 | `POST /api/quota/accounts/:n/{rotate,retire}` HTTP | P2 | closed | claude-host | dispatch via CommandBus |
| hq-fe-api-w.11 | `POST /api/beads/bulk` + rate-limit | P3 | closed | — | atomic bulk-create con per-actor rate-limit middleware en sub-router; valida ítems contra mismas reglas que `POST /api/beads`; refusa todo el batch on primer fallo |

### Epic `hq-fe-rbac` — perfiles, JWT, scopes

| Bead | Título | Pri | Estado | Agente | Notas |
|---|---|---|---|---|---|
| hq-fe-rbac.1 | JWT signing en gt-api (HS256 decidido) | P1 | closed | claude-host | HS256 (single binary issues+verifies; RS256 deferred a multi-verifier); `jwt.rs` (`Claims{sub,iss,iat,exp,roles,scopes}` + `JwtIssuer{sign,verify}`); `AuthConfig::Jwt{issuer}` + `AuthClaims` request ext; middleware verifica firma/exp/iss y propaga claims; `/api/whoami` ahora reporta `mode=jwt` + roles/scopes desde claims; main.rs prioridad `GT_WEB_JWT_SECRET` > `GT_WEB_TOKEN` > `GT_WEB_AUTH=disabled`; 5 cargo tests (jwt module) + 4 middleware tests + 2 whoami integration tests |
| hq-fe-rbac.2 | `roles.toml` unificado con `mcp-scope.toml` | P1 | closed | claude-host | nuevo crate kernel `gt-rbac` con `RbacConfig{actors, roles}` (TOML/JSON); gt-mcp `ScopeConfig` = alias de `RbacConfig` (back-compat via `ResolveScope` trait); gt-web re-exporta `RbacConfig` + `JwtIssuer::with_rbac()/sign_for_actor(actor)`; main.rs lee `GT_WEB_RBAC_CONFIG` (fallback `GT_MCP_SCOPE_CONFIG`); `deploy/mcp-scope.toml` extendido con sample `[roles.*]`; 7 gt-rbac tests + 3 jwt sign_for_actor + 1 whoami integration |
| hq-fe-rbac.3 | Middleware per-scope en gt-web | P1 | closed | claude-host | nuevo módulo `scope.rs` (`ScopeGuard` + `scope_middleware`) en `gt-web`; `route_layer(from_fn_with_state(...))` por ruta en `router_with_stores` con scopes (`sessions.{read,write}`, `beads.{read,write}`, `merge.read`, `worktrees.read`, `nudge.write`, `convoys.{read,write}`, `quota.{read,write}`, `feed.read`); JWT mode gateado contra `AuthClaims.scopes`, Bearer/Open mode grandfathered (sin claims); `WebAuditEvent::Forbidden{actor,method,path,scope}` + `web.forbidden` audit kind; `/api/whoami` sin guard (identity bootstrap); 4 scope tests cubren pass/deny/audit/empty + grandfather paths |
| hq-fe-rbac.4 | `GET /api/whoami` (actor + roles + scopes) | P1 | closed | claude-host | `Actor` newtype en request ext via auth middleware (open=`web:open`, bearer=`actor_tag`); `WhoamiDto {actor, mode, roles, scopes}` (roles/scopes empty hasta rbac.{1,2,3}); 3 cargo tests; `lib/{types,api}/whoami.ts` + `+layout.svelte` hidrata `auth.hydrate(whoami)` con skip401Hook |
| hq-fe-rbac.5 | Enriquecer `web.invoked` con command+target | P2 | closed | claude-host | `Invoked` lleva `command: Option<String>` + `target: Option<String>` (serde `skip_serializing_if=Option::is_none` para back-compat del jsonl); `scope::RouteContext{command,target}` se inyecta en response extensions desde `scope_middleware` (accept path), `auth_middleware` lo lee post-handler con `route_context_of(&resp)` para stampear `Invoked` enriquecido; `command` = scope literal (e.g. `beads.write`), `target` = id de recurso vía `target_from_path("/api/<col>/<id>(/...)")`; Bearer/Open y rutas sin guard (`/api/whoami`) emiten `None/None`; 4 unit tests (target_from_path happy/None + jwt-mode end-to-end + bearer-mode None) |

### Epic `hq-fe-auth` — account login pty driver

| Bead | Título | Pri | Estado | Agente | Notas |
|---|---|---|---|---|---|
| hq-fe-auth.0 | Research Anthropic OAuth client_id + redirect | P2 | open | — | descartar/perseguir opción A |
| hq-fe-auth.1 | PTY driver: spawn `claude /login` + URL regex | P1 | closed | claude-host | nuevo crate `gt-login` en `domain/orchestration/`: `Pty` port + `PortablePty` (portable-pty 0.8) + `FakePty` scripted; `LoginDriver::run` síncrono (3 fases: read-until-URL, request token, write+wait); `extract_url` regex anclada a `console.anthropic.com`/`claude.ai` con trim de `.,)]>"'`; `LoginEvent {Started, UrlReady{url}, Complete{account}, Failed{reason}}` + `LoginFailure {Spawn, UrlMissing, TokenRejected{status}, Cancelled, Io}` listos para SSE `quota.login_*` en `hq-fe-auth.3`; 11 tests (5 regex + 1 state + 5 driver vía FakePty cubriendo happy/url-split/eof/cancel/exit-nonzero) |
| hq-fe-auth.2 | `POST /api/quota/accounts/:n/login` + token + cancel | P1 | closed | claude-host | módulo `gt-web::login` consume `gt_login::LoginDriver` desde `tokio::task::spawn_blocking`; 3 rutas POST (`start` → 202 + flow_id, `/token` → wake driver, `/cancel` → drop slot); `LoginRegistry` (HashMap por account) bloquea segundo start con 409 que devuelve el `flow_id` vivo para re-attach; bridge sync ↔ async vía `std::sync::mpsc` (drop tx = `LoginFailure::Cancelled`); emite `quota.login_{started,url_ready,complete,failed}` `EventRecord`s en el broadcast del root (correlation_id = flow_id) — `.3` define SSE encoding; scope `quota.write`; `GT_LOGIN_ENABLE=1` activa `PortablePty` en prod, ausente → 503 limpio; `GT_LOGIN_CMD`/`GT_LOGIN_ARGS` parametrizan spawn; 6 e2e tests con `FakePty` (start+202+url_ready, 409+flow_id, token→complete, cancel→failed-cancelled, wrong-flow-id→404, no-pty→503) + 5 unit tests del registry + emit_event |
| hq-fe-auth.3 | SSE kinds `quota.login_*` | P1 | closed | claude-host | DTO canónica en `gt-web::dto`: `QuotaLogin{Started,UrlReady,Complete,Failed}` structs (sin `kind` redundante en payload — discriminador vive en `EventRecord.type`) + envelope `QuotaLoginEvent` con `impl EventKind`; `login::emit_event` refactor para construir vía DTO (lockstep wire ↔ Rust); espejo TS en `apps/web/src/lib/types/quota-login.ts` con `LoginFailure` discriminado + `QuotaLoginEventPayload` union; SSE bridge ya forwardeaba `EventRecord` crudo, ergo no toca `stream.rs`; e2e test confirma `/api/stream` entrega `started → url_ready → complete` en orden lexical + payload `url_ready` decodifica typed `QuotaLoginUrlReady` |
| hq-fe-auth.4 | Timeout + cleanup pty zombis + lock per account | P2 | open | — | mitigación |

### Epic `hq-fe-skills` — skills + roles domain (nuevo)

| Bead | Título | Pri | Estado | Agente | Notas |
|---|---|---|---|---|---|
| hq-fe-skills.1 | Domain crate `gt-skills` (catalog + role binding) | P2 | open | — | event-sourced; persistencia Dolt |
| hq-fe-skills.2 | `GET /api/skills` + `GET /api/roles` (+ skills habilitadas) | P2 | open | — | |
| hq-fe-skills.3 | `POST /api/roles/:role/skills` toggle (validate+execute) | P2 | open | — | |
| hq-fe-skills.4 | Map skills → MCP scope additions (config dinámica) | P2 | open | — | reload mcp-scope sin restart |
| hq-fe-skills.5 | Reload signal cuando se cambian skills | P3 | open | — | broadcast roles.changed |

### Epic `hq-fe-term` — terminal bridge

| Bead | Título | Pri | Estado | Agente | Notas |
|---|---|---|---|---|---|
| hq-fe-term.0 | **Spike obligatorio**: WS en gt-api · MCP tool · bin separado | P2 | DECIDIDO 2026-05-30 → WS en gt-web ([doc](spike-hq-fe-term-0-transport.md)) | sheriff | substrate: `gt-login::pty` + `gt-polecat::tmux` |
| hq-fe-term.1 | PTY adapter en gt-api (post-decision) | P2 | open · DESBLOQUEADO | — | use `tmux pipe-pane` (read) + `send-keys` (write); fallback `portable-pty` |
| hq-fe-term.2 | WebSocket `/api/sessions/:id/term` (o equivalente) | P2 | open · DESBLOQUEADO | — | en gt-web; scope `terminal.attach`; per-method router (no `route_layer`) |
| hq-fe-term.3 | Structured stream (kind: code/comment/highlight/warn/raw) | P3 | open | — | derivar de Claude output o passthrough |

### Epic `hq-fe-build` — scaffold + tooling

| Bead | Título | Pri | Estado | Agente | Notas |
|---|---|---|---|---|---|
| hq-fe-build.1 | Scaffold `apps/web/` (svelte5 + adapter-static + tailwind + pnpm) | P1 | **closed** | codecsrayo | landed 2a76dffc; pnpm install/check/build verdes |
| hq-fe-build.2 | `lib/api` client wrapper (fetch + bearer + idem-key) | P1 | closed | claude-host | `lib/api/client.ts` (apiGet/apiSend/apiRequest + ApiError + setOn401); inyecta Bearer (skip dev sentinel), Idempotency-Key auto en non-GET, 401 → +layout.svelte hook (clearBearer+goto /login); refactor de issues/sessions/worktrees/beads (10 vitest) |
| hq-fe-build.3 | `lib/sse` stream + router (fan-out por kind) | P1 | closed | claude-host | `lib/sse.ts` SseRouter singleton; `subscribe(kind, h)` + `subscribeStatus(h)`; exact + `domain.*` + `*` patterns; lazy open + auto-close last-sub; 10 vitest cases con FakeEventSource |
| hq-fe-build.4 | `lib/stores` base con runes (auth, sessions, beads, activity, quota) | P1 | closed | claude-host | `stores/{sessions,beads,activity,quota}.svelte.ts` singletons + 23 vitest cases; auth + theme ya estaban en view.1/.12 |
| hq-fe-build.5 | `lib/types` DTOs manuales desde frontend-api-surface | P1 | open | — | regenerar de JsonSchema si crece |
| hq-fe-build.6 | Vite proxy + dev workflow doc | P2 | closed | claude-host | `apps/web/docs/dev-workflow.md`: daily loop, proxy table, host-side gt-web + Dolt-mode, troubleshooting matrix; README + docs/README link in |
| hq-fe-build.7 | Vitest (stores/logic) + Playwright (e2e) bootstrap | P2 | open | — | |
| hq-fe-build.8 | CI lint + build gate (svelte-check estricto) | P2 | closed | claude-host | `.github/workflows/web-ci.yml` (pnpm 9 + node 22 · install → check → eslint → vitest → build); `.prettierignore` skips docs/lock; prettier dropped pending plugin 3.5 + prettier 3.8 compat fix |

### Epic `hq-fe-view` — vistas + componentes

| Bead | Título | Pri | Estado | Agente | Notas |
|---|---|---|---|---|---|
| hq-fe-view.1 | Layout raíz: Shell + Sidebar + Topbar + Dock + theme toggle | P1 | closed | claude-host | `lib/stores/theme.svelte.ts` + `lib/components/{layout/{Shell,Sidebar,Topbar,Dock,TabStrip,StubView},theme/ThemeToggle}.svelte`; 7 placeholder routes + landing hub |
| hq-fe-view.2 | `/login` route + bearer guard en `+layout.ts` | P1 | closed | claude-host | `routes/login/+page.svelte` paste + dev sentinel; `+layout.ts` LayoutLoad redirige 307 a `/login` si falta bearer; ProfileMenu logout ahora `goto('/login')` |
| hq-fe-view.3 | Activity view (feed + cat filter + rig filter + recent peek) | P1 | closed | claude-host | `routes/activity/+page.svelte` + `lib/event-category.ts` (5 buckets · 6 vitest); SSE subscribe '*' → `activity` store; cat/rig/text filters + auto-scroll + status pill; hidrato hist pendiente api-r.5 |
| hq-fe-view.4 | Sessions view (table + filters + kill DangerButton) | P1 | closed | claude-host | `lib/{types,api}/session*` + tabla con role/rig/state filters + per-role tint; Kill DangerButton disabled hasta api-w.6 |
| hq-fe-view.5 | Work view (kanban 5 cols + drag-drop + DangerZone close) | P1 | closed | claude-host | `lib/{types/bead,api/beads,kanban}.ts` + `routes/work/+page.svelte`; `svelte-dnd-action@0.9.69` + optimistic drag → POST transition → revert+refresh on 4xx; close via DangerZone typed-name → `done` (4 vitest cubren operator matrix 1:1 con gt-web) |
| hq-fe-view.6 | Convoys view (list + e-stop DangerZone) | P2 | closed | claude-host | `routes/convoys/+page.svelte` + `lib/api/convoys.failConvoyMember`; agrupa por convoy con state filter; per-member Fail abre `DangerZone` typed-name + reason input → `POST /api/convoys/:c/members/:m/fail` (hq-fe-api-w.9); SSE `orch.*` dispara refetch debounceado (in-flight + pending flag). 4 vitest cubren GET path/encoding + POST body/idem-key/path-encoding |
| hq-fe-view.7 | Merge Q view | P2 | open | — | |
| hq-fe-view.8 | Crew view (RoleList + RolePanel + SkillToggle + ScopeMatrix) | P2 | open | — | depende hq-fe-skills |
| hq-fe-view.9 | Rigs view | P3 | open | — | |
| hq-fe-view.10 | Quota sidebar (AccountCard + Meter + RotationChips + LoginBtn) | P1 | closed | claude-host | `lib/components/quota/{QuotaPanel,AccountCard,QuotaMeter,RotationChips,LoginBtn}.svelte` + `lib/quota-meter.ts` (tone/ratio/countdown/group); hidrata `GET /api/quota/accounts` (hq-fe-api-r.1) + `GET /api/quota/rotation` y refetch ambos por `quota.*` SSE; sidebar w-48→w-56 + overflow-auto; LoginBtn disabled hasta hq-fe-auth.1; 22 vitest (meter 11 + api 4 + store 7 ya existían) |
| hq-fe-view.11 | Dock terminal shell (mount + tabs + xterm lazy) | P2 | open | — | bloqueada por hq-fe-term decision |
| hq-fe-view.12 | `<Guard>`, `<DangerButton>`, `<DangerZone>` components | P1 | closed | claude-host | `lib/stores/auth.svelte.ts` (dev permissive · live hydrate · readOnly) + `lib/components/auth/{Guard,DangerButton,DangerZone}.svelte` + extracted `danger-button.ts` state machine (12 vitest); `/design` playground route |
| hq-fe-view.13 | Profile menu topbar (whoami + read-only toggle + logout) | P1 | closed | claude-host | `lib/components/auth/{ProfileBadge,ProfileMenu}.svelte` + `lib/bearer.ts` (3 vitest); dropdown click-outside + esc; wired into Topbar replacing placeholder |
| hq-fe-view.14 | Worktrees view (SCM-like panel: branches+dirty+ahead/behind por agente) | P1 | closed | claude-host | route `/worktrees` + `lib/{api,types,claim-branch}` + vitest; bead badge desde `claim/<bead-id>` convención |
| hq-fe-view.15 | Worktrees panel cross-link real: bead title+assignee desde /api/issues | P1 | closed | claude-host | `Promise.allSettled([worktrees,issues])`; `issuesById` derived map; `open,working` slice; badge tooltip = título |
| hq-fe-view.16 | Worktrees panel: ocultar idle por default (clean + no claim/ + no main) | P2 | closed | claude-host | `lib/worktree-filter.ts::isActive` + 5 vitest cases; counter `X active · Y idle hidden` en header |
| hq-fe-view.17 | Worktrees panel: HEAD subject + author bajo branch | P2 | closed | claude-host | render `subject — author` faint line; truncate + tooltip full text |
| hq-fe-view.18 | Worktrees panel: sort active por HEAD time desc + relative age chip | P2 | closed | claude-host | `lib/relative-time.ts` (`relativeAge` + `byRecency`); 7 vitest cases; main pinned top |
| hq-fe-view.19 | Worktrees panel: EventSource SSE en vez de setInterval poll | P2 | closed | claude-host | `EventSource('/api/worktrees/stream')`; auto-reconnect; issues sigue poll 12s |

### Epic `hq-fe-cut` — cutover

| Bead | Título | Pri | Estado | Agente | Notas |
|---|---|---|---|---|---|
| hq-fe-cut.1 | gt-api sirve assets estáticos del build (`/` y `/_app/*`) | P1 | closed | deacon | `tower-http::services::ServeDir` montado como `Router::fallback_service` en `gt-web` (lib.rs `with_static_assets`). SPA history-mode fallback a `index.html`. `GT_WEB_DIST` overridea path; vacío desactiva. 5 tests en `tests/static_assets.rs` (root, `/_app/immutable/*`, deep path, /api 401, dist ausente) |
| hq-fe-cut.2 | Traefik / compose validación: `gastown.codecsrayo.com` → SPA | P1 | closed | claude-host | gt-api image hornea `apps/web/build` vía stage `web-builder` (node:20 + pnpm + vite); compose pasa build context al repo root; `GT_WEB_DIST=/srv/web`. Smoke vía traefik con `--resolve`: `/`, `/_app/version.json`, `/login/`, `/sessions/`, `/health`, `/readyz` 200; `/api/quota/accounts` 401 sin token / 200 con bearer (59b53921) |
| hq-fe-cut.3 | Borrar `internal/web/` del árbol (limpieza Go) | P2 | deferred → hq-oap5 | — | reubicado bajo el epic `hq-oap5` (Retire Go orchestrator); borrar el dir cascadeaba edits a `internal/cmd/dashboard.go` + `internal/cmd/quota.go`, mejor bundle con el resto del Go retirement |
| hq-fe-cut.4 | Docs ops (token, RBAC bootstrap, troubleshooting) | P2 | closed | claude-host | nuevo `apps/api/docs/deployment/06-ops-runbook.md`: auth modes + bootstrap (bearer / JWT + RBAC `mcp-scope.toml`), rotación, troubleshooting (traefik / SPA / `/api/*` 401-403 / Dolt / PG), smoke recipe; index en `apps/api/docs/README.md`; cross-link desde `07-frontend.md` |

---

## Grafo de dependencias

```
hq-fe-api-w.1 (command-bus)
  ├── hq-fe-api-w.2 (idem-key)
  │     ├── hq-fe-api-w.3..11 (HTTP write routes)
  │     ├── hq-fe-rbac.3 (per-scope middleware)
  │     └── hq-fe-auth.2 (login HTTP)
  └── hq-fe-skills.3 (toggle endpoint)

hq-fe-rbac.1 (JWT) ── hq-fe-rbac.2 (roles.toml) ── hq-fe-rbac.3 ── hq-fe-rbac.4 (whoami)
                                                                       └── hq-fe-view.* (guards)

hq-fe-build.1 (scaffold)
  ├── hq-fe-build.2..4 (api/sse/stores)
  └── hq-fe-view.1 (layout)
        └── hq-fe-view.2..13 (resto en paralelo)

hq-fe-term.0 (spike) ── hq-fe-term.1..3 ── hq-fe-view.11 (dock)

hq-fe-view 80%+ done ── hq-fe-cut.1..4
```

---

## Riesgos consolidados

| Riesgo | Mitigación |
|---|---|
| Dos backends a la vez (Go viejo + Rust nuevo) | Falso — Go ya retirado del despliegue (`c877758e`); solo está en árbol como referencia |
| Agente confunde la API vieja con la nueva | [frontend-api-surface.md](frontend-api-surface.md) es spec, no `internal/web/` |
| OAuth account login bloqueado por Anthropic | Plan B (pty driver) viable sin coordinación externa |
| Terminal bridge ambicioso | Diferido tras spike; resto del MVP no depende de él |
| Skills domain nuevo | Pequeño event-sourced; encaja en patrón existente |
| RBAC granular escope | `mcp-scope.toml` ya existe → reuso |
| Múltiples agentes pisándose en este epic | Tabla de estado arriba es lock cooperativo |

## Resumen visual

```
Fase 0 ── fundación ─────────  command-bus · idem-key · scaffold · types
Fase 1 ── read-side mínima ──  snapshots · JWT/whoami · api/sse/stores
Fase 2 ── write-side mínima ─  beads CRUD · session kill · login + Guard
Fase 3 ── vistas core ──────  Activity · Sessions · Work · Profile menu
Fase 4 ── avanzadas + auth ──  Skills · Convoys · Crew · pty login
Fase 5 ── terminal + cut ───  spike → dock · serve build · borrar Go
```
