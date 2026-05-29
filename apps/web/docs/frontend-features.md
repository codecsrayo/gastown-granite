# Catálogo de features del frontend

Una entrada por feature: qué hace, cómo se ve, qué endpoints usa, qué scope
RBAC requiere, qué eventos SSE consume, qué bead la implementa. Sirve como
índice navegable para agentes que entran a una región del dashboard sin
contexto previo.

> **Para agentes:** si una feature está en este catálogo pero no tiene API
> en [frontend-api-surface.md](frontend-api-surface.md), está bloqueada por
> el gap correspondiente. **No improvises endpoints.**

Cross-refs base:

- API: [frontend-api-surface.md](frontend-api-surface.md)
- Plan: [frontend-migration-sveltekit.md](frontend-migration-sveltekit.md)
- Arq: [frontend-architecture.md](frontend-architecture.md)
- Wireframe: [Gas Town Redesign Wireframes.html](Gas%20Town%20Redesign%20Wireframes.html)

---

## 1 · Login + bearer

| | |
|---|---|
| **Qué hace** | Usuario pega JWT en `/login`, dashboard lo guarda en localStorage y bootstrappea sesión |
| **Endpoints** | `GET /api/whoami` (hidrata roles+scopes) |
| **SSE** | — (suscribe tras login) |
| **Scope RBAC** | público (no-op antes de login) |
| **Beads** | hq-fe-rbac.4 · hq-fe-view.2 · hq-fe-build.2 |
| **Componentes** | `routes/login/+page.svelte` · `lib/stores/auth.svelte.ts` |
| **Notas** | Pre-RBAC (Fase 1): bearer plano. Post-RBAC (Fase 2): JWT con claims. Mismo UI, distinta payload. Futuro: OAuth real (descartado por bloqueador Anthropic — ver [hq-fe-auth.0]). |

## 2 · Sidebar · Account Quota (panel izquierdo)

| | |
|---|---|
| **Qué hace** | Lista cuentas Claude (brayan, fsrb, codecsrayo, a407) agrupadas en ACTIVE / INACTIVE / BLOCKED; meter de tokens; reset_at; tags de sesiones que usan cada cuenta; botón Login por cuenta |
| **Endpoints** | `GET /api/quota/accounts` (snapshot inicial) · `POST /api/quota/accounts/:n/login` (Fase 4) · `POST /api/quota/accounts/:n/rotate` |
| **SSE** | `quota.tokens_sampled` · `quota.account_limited` · `quota.window_reset` · `quota.rotated` · `quota.login_*` |
| **Scope RBAC** | `quota.read` (snapshot) · `quota.rotate` (botón rotate) · `quota.login` (botón Login) |
| **Beads** | hq-fe-api-r.1 · hq-fe-api-w.10 · hq-fe-auth.* · hq-fe-view.10 |
| **Componentes** | `features/quota/AccountCard.svelte` · `QuotaMeter.svelte` · `RotationChips.svelte` · `LoginFlow.svelte` |
| **Notas** | Sidebar persistente (en `+layout.svelte`); nunca remonta. Visual canon: [pagina.png](pagina.png). Meter warn cuando >75%; bad cuando rate-limited. |

## 3 · Tab Activity (canon hero · imagen)

| | |
|---|---|
| **Qué hace** | Feed live de eventos del bus (SSE) + histórico opcional (`/api/feed?since=`); filtros por categoría (agent/work/quota/system/audit) y rig |
| **Endpoints** | `GET /api/feed?since=1h` (snapshot histórico) |
| **SSE** | **todos** los kinds (`agent.*`, `merge.*`, `patrol.*`, `orch.*`, `quota.*`, `scheduling.*`, `rig.*`, `web.*` filtrado a categoría audit) |
| **Scope RBAC** | `feed.read` (= todos los `*.read`) |
| **Beads** | hq-fe-api-r.5 · hq-fe-view.3 |
| **Componentes** | `features/activity/ActivityFeed.svelte` · `EventRow.svelte` · `CategoryFilter.svelte` |
| **Notas** | Ring buffer client-side (~500 records); auto-scroll opt-in. Vista por defecto al entrar (= imagen canónica). Filtro "audit" muestra solo `web.*` / `mcp.*` (quién hizo qué). |

## 4 · Tab Sessions

| | |
|---|---|
| **Qué hace** | Tabla de sesiones vivas (polecats + dogs + mayor); filtros por rig + role; click sesión → detalle + acciones (Kill, Restart, Interrupt) |
| **Endpoints** | `GET /api/sessions?rig=…&role=…` · `DELETE /api/sessions/:id` · `POST /api/sessions/:id/restart` · `POST /api/sessions/:id/interrupt` |
| **SSE** | `agent.spawned` · `agent.killed` · `agent.heartbeat` · `agent.session_end` · `agent.transition` |
| **Scope RBAC** | `session.read` · `session.kill` · `session.restart` · `session.interrupt` |
| **Beads** | hq-fe-api-r.6 · hq-fe-api-w.6..8 · hq-fe-view.4 |
| **Componentes** | `features/sessions/SessionsTable.svelte` · `SessionRow.svelte` · `KillConfirm.svelte` (DangerButton armable) |
| **Notas** | Kill = SIGTERM con timeout → SIGKILL si no exit. Interrupt = `tmux send-keys ESC` (Claude interrumpe stream, sesión sigue viva). Restart = kill + respawn con misma crew. |

## 5 · Tab Work (kanban)

| | |
|---|---|
| **Qué hace** | Beads agrupados en 5 columnas por status (pending · dispatched · working · done · failed); drag entre columnas = transition; click card = detalle + comments + close |
| **Endpoints** | `GET /api/beads?status=…` · `POST /api/beads` (create) · `PATCH /api/beads/:id` · `POST /api/beads/:id/transition` · `POST /api/beads/:id/comments` |
| **SSE** | `scheduling.dispatched` · `scheduling.dispatch_failed` · evento futuro `bead.transitioned` (junto con hq-fe-api-w.4) |
| **Scope RBAC** | `bead.read` · `bead.create` · `bead.update` · `bead.transition` · `bead.close` |
| **Beads** | hq-fe-api-r.* · hq-fe-api-w.3..5 · hq-fe-view.5 |
| **Componentes** | `features/work/KanbanBoard.svelte` · `Column.svelte` · `BeadCard.svelte` · `DragHandle.svelte` |
| **Notas** | State-machine backend rechaza ilegales (e.g. `done → working`) → 409 → revert. Drag library: `svelte-dnd-action`. Close requiere DangerZone (typed bead id). |

## 6 · Tab Convoys

| | |
|---|---|
| **Qué hace** | Lista de convoys (orquestaciones multi-bead); ver miembros + progreso; e-stop (fail) un convoy entero o un miembro |
| **Endpoints** | `GET /api/convoys` · `POST /api/convoys` (launch) · `POST /api/convoys/:id/pause` · `POST /api/convoys/:id/resume` · `POST /api/convoys/:id/members/:m/fail` |
| **SSE** | `orch.convoy_created` · `orch.convoy_launched` · `orch.convoy_closed` · `orch.convoy_failed` · `orch.member_*` |
| **Scope RBAC** | `convoy.read` · `convoy.launch` · `convoy.pause` · `convoy.fail` (admin-only) |
| **Beads** | hq-fe-api-r.3 · hq-fe-api-w.9 · hq-fe-view.6 |
| **Componentes** | `features/convoys/ConvoyList.svelte` · `ConvoyDetail.svelte` · `EStopZone.svelte` (DangerZone typed-name) |
| **Notas** | Pause/resume es nuevo dominio (eventos `orch.convoy_paused` / `orch.convoy_resumed` a añadir junto con hq-fe-api-w.9). E-stop = 2-step confirm con tipear nombre exacto del convoy. |

## 7 · Tab Merge Q

| | |
|---|---|
| **Qué hace** | Visualiza merge slots (READY / IN_PROGRESS / MERGED / FAILED) + cola; permite forzar complete/fail desde admin |
| **Endpoints** | `GET /api/merges` · `POST /api/merges/:id/{submit,complete,fail}` |
| **SSE** | `merge.ready` · `merge.started` · `merge.merged` · `merge.failed` |
| **Scope RBAC** | `merge.read` · `merge.complete` · `merge.fail` |
| **Beads** | hq-fe-api-r.4 · hq-fe-view.7 |
| **Componentes** | `features/merge/MergeBoard.svelte` · `MergeSlot.svelte` |
| **Notas** | Refinery normalmente gestiona; dashboard sirve para inspección + override manual cuando refinery cae. |

## 8 · Tab Crew (roles + skills)

| | |
|---|---|
| **Qué hace** | Catálogo de 6 roles (mayor/deacon/refinery/witness/sheriff/polecat); por rol: skills habilitadas (toggle), MCP scope allow/deny matrix, sesiones vivas con ese rol |
| **Endpoints** | `GET /api/roles` · `GET /api/roles/:role/scope` · `GET /api/skills` · `POST /api/roles/:role/skills` (toggle) |
| **SSE** | evento futuro `roles.changed` (junto con hq-fe-skills.5) |
| **Scope RBAC** | `role.read` · `role.assign` (admin-only) · `skill.toggle` (admin-only) |
| **Beads** | hq-fe-skills.* · hq-fe-view.8 |
| **Componentes** | `features/crew/RoleList.svelte` · `RolePanel.svelte` · `SkillToggle.svelte` · `ScopeMatrix.svelte` |
| **Notas** | Skill toggle modifica config dinámicamente (recarga `mcp-scope.toml`). Dominio NUEVO — crate `gt-skills` event-sourced. Cambios auditados. |

## 9 · Tab Rigs

| | |
|---|---|
| **Qué hace** | Catálogo de rigs (gastown_granite, plane); ver default branch, prefix, remotes; admin puede `rig.add/adopt/remove/set_prefix/set_default_branch` |
| **Endpoints** | `GET /api/rigs` (promover desde MCP `gt://rigs`) · `POST /api/rigs` · `PATCH /api/rigs/:n` |
| **SSE** | `rig.added` · `rig.removed` · `rig.prefix_changed` · `rig.default_branch_changed` |
| **Scope RBAC** | `rig.read` · `rig.add` · `rig.update` |
| **Beads** | hq-fe-api-r.* (follow-up) · hq-fe-view.9 |
| **Componentes** | `features/rigs/RigsTable.svelte` |
| **Notas** | MVP read-only; mutación en follow-up (admin rara). |

## 10 · Topbar · Profile menu

| | |
|---|---|
| **Qué hace** | Chip con actor + role; dropdown con read-only toggle, logout, switch profile (futuro multi-actor) |
| **Endpoints** | `GET /api/whoami` (al boot) |
| **SSE** | — |
| **Scope RBAC** | público (cualquiera logueado) |
| **Beads** | hq-fe-view.13 · hq-fe-rbac.4 |
| **Componentes** | `components/auth/ProfileBadge.svelte` · `ProfileMenu.svelte` |
| **Notas** | Read-only toggle = downgrade voluntario (admin se vuelve observer temporalmente — evita fat-finger). No persistente entre tabs (señal explícita). |

## 11 · Dock terminal (xterm)

| | |
|---|---|
| **Qué hace** | Panel inferior fijo con tabs por sesión tmux; attach a la sesión activa para ver stream y enviar input; chips de "waiting on unlock" + "last rotation" |
| **Endpoints** | TBD post-spike (`hq-fe-term.0`): WS `/api/sessions/:id/term` o MCP tool |
| **SSE** | `quota.rotated` (para chips de rotation status) |
| **Scope RBAC** | `terminal.attach` (operator+) |
| **Beads** | hq-fe-term.* · hq-fe-view.11 |
| **Componentes** | `features/terminal/XtermWrap.svelte` · `TermTabs.svelte` · `TermPrompt.svelte` |
| **Notas** | **BLOQUEADA** hasta spike `hq-fe-term.0` decida transport. xterm.js carga lazy (~150kb) solo al abrir dock. Pop-out window = follow-up. |

## 12 · Theme toggle (dark/light)

| | |
|---|---|
| **Qué hace** | Switch entre dark (canónico) y light; persiste en localStorage |
| **Endpoints** | — |
| **SSE** | — |
| **Scope RBAC** | público |
| **Beads** | hq-fe-view.1 |
| **Componentes** | `components/theme/ThemeToggle.svelte` |
| **Notas** | CSS vars en `[data-theme]`. Dark = canon (= imagen). Light disponible para preferencia personal. |

## 13 · Account login (pty driver)

| | |
|---|---|
| **Qué hace** | Botón "Login" en account card → arranca `claude /login` en pty backend → SSE devuelve URL → dashboard abre `window.open(url)` → usuario autentica en anthropic.com → ve token → pega en input dashboard → dashboard POST → backend escribe a pty stdin → token guardado |
| **Endpoints** | `POST /api/quota/accounts/:n/login` (start) · `POST /api/quota/accounts/:n/login/:id/token` (paste) · `DELETE` (cancel) |
| **SSE** | `quota.login_started` · `quota.login_url_ready` · `quota.login_complete` · `quota.login_failed` |
| **Scope RBAC** | `quota.login` (maintainer+) |
| **Beads** | hq-fe-auth.* |
| **Componentes** | `features/quota/LoginFlow.svelte` (modal multi-paso) |
| **Notas** | UX: 1 click + 1 paste = 2 acciones (vs 3 actuales). Bloqueador raíz: Claude Code no expone callback HTTP (option A). Mitigación: pty driver (option B, viable). Ver detalle en respuesta a feature `auth desde dashboard`. |

## 14 · Audit feed (sub-vista de Activity)

| | |
|---|---|
| **Qué hace** | Filtro "audit" en Activity tab muestra cada `web.invoked` enriquecido con `actor + command + target` → "brayan killed gg-furiosa · 2s ago" |
| **Endpoints** | `GET /api/feed?cat=audit&since=…` |
| **SSE** | `web.invoked` · `web.unauthorized` · `mcp.invoked` · `mcp.unauthorized` |
| **Scope RBAC** | `audit.read` (operator+) |
| **Beads** | hq-fe-rbac.5 · hq-fe-view.3 |
| **Componentes** | `features/activity/AuditFeed.svelte` |
| **Notas** | Útil post-incidente: "¿quién mató mi sesión?". Backend ya emite `web.invoked`; falta enriquecer con command + target (hq-fe-rbac.5). |

---

## Matriz: feature × fase

| Feature | F0 | F1 | F2 | F3 | F4 | F5 |
|---|---|---|---|---|---|---|
| 1 Login | | ● | | | | |
| 2 Quota sidebar | | ● snap | | | ● login btn | |
| 3 Activity | | | | ● | | |
| 4 Sessions | | | ● kill | ● table | | |
| 5 Work kanban | | | | ● | | |
| 6 Convoys | | | | | ● | |
| 7 Merge Q | | | | | ● | |
| 8 Crew (skills) | | | | | ● | |
| 9 Rigs | | | | | (●) | |
| 10 Profile menu | | | | ● | | |
| 11 Dock terminal | | | | | | ● post-spike |
| 12 Theme toggle | | | | ● | | |
| 13 Account login | | | | | ● | |
| 14 Audit feed | | | | | ● | |

## Decisiones de scope (NO HACER en MVP)

| Feature solicitada | Decisión | Razón |
|---|---|---|
| Mail / inbox tab | **descartado** | Dominio no existe en Rust; no se reimplementa |
| Git events stream | **diferido** | No bloquea operación; abrir bead cuando sea necesario |
| Hooks tab | **diferido** | Surface via Activity feed |
| Dogs tab (separado) | **diferido** | Overlap con Sessions filtrado por role |
| Polecats tab (separado) | **diferido** | Overlap con Sessions filtrado |
| Escalations tab dedicada | **diferido** | Activity feed con filtro escalation cubre |
| Pop-out terminal | **diferido** | Bloqueador hq-fe-term primero |
| Command palette `⌘` | **diferido** | Útil pero no MVP; abrir post-cutover |
| Spark-timeline de eventos | **diferido** | Visualización fancy; Activity tabular basta |
| Multi-actor session local | **diferido** | 1 token = 1 actor por device; multi = follow-up |

## Glosario rápido

| Término | Significado |
|---|---|
| **bead** | Issue/task; unidad de trabajo en `bd` / Dolt `issues` |
| **convoy** | Orquestación de múltiples beads relacionados (multi-step) |
| **rig** | Repo + identidad (`gastown_granite`, `plane`); tiene prefix + default branch |
| **polecat** | Sesión Claude Code corriendo dentro de tmux, asignada a una crew |
| **crew** | Identidad del agente (`brayan`, `nux`, `furiosa`, …) corriendo en un polecat |
| **dog** | Supervisor (deacon, refinery, witness, overseer, sheriff) — no es polecat |
| **mayor** | Top-level orquestador que delega convoys |
| **wisp** | Lifecycle event "fantasma" (convoy-complete, handoff…) — ruido operacional |
| **patrol** | Mecanismo de heartbeat por rol con leases que expiran |
| **merge slot** | Posición en el state machine de merges (READY/IN_PROGRESS/MERGED) |
| **quota account** | Cuenta Anthropic con cuota y créditos; sesiones se asignan a una cuenta |
| **scope** | Permiso fino (e.g. `session.kill`, `bead.read`) — del JWT del actor humano |
| **role** | Conjunto de scopes (observer / operator / maintainer / admin); del JWT |
