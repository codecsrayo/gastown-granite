# Frontend ↔ Rust API surface

Snapshot of the **actual** HTTP/SSE/MCP surface the Rust backend exposes today
(`apps/api/`). This is the contract the new SvelteKit frontend builds against.
The retired Go dashboard (`internal/web/`) is **not** the spec — its endpoints
do not exist in Rust and will not be ported one-for-one.

- Authoritative source for routes: [bins/gt-web/src/lib.rs](../../api/crates/bins/gt-web/src/lib.rs)
- DTOs: [bins/gt-web/src/dto.rs](../../api/crates/bins/gt-web/src/dto.rs)
- SSE: [bins/gt-web/src/stream.rs](../../api/crates/bins/gt-web/src/stream.rs)
- MCP tools/resources: [bins/gt-mcp/src/service.rs](../../api/crates/bins/gt-mcp/src/service.rs)
- Deploy topology: [docker-compose.yml](../../../docker-compose.yml)

If a route or DTO changes, update this file in the same commit.

## Topology (compose)

| Service | Image | Bind | Exposed | Purpose |
|---|---|---|---|---|
| `gt-api` | apps/api build | `0.0.0.0:8787` | traefik → `gastown.codecsrayo.com` | REST + SSE + health/readyz/metrics |
| `gt-mcp` | apps/api build | `0.0.0.0:8765` | `127.0.0.1:8765` (host) | MCP streamable-HTTP (tools + resources) |
| `gt` | orchestrator | `0.0.0.0:9100` (metrics) | compose net | reactor + daemons; `/metrics` only |
| `dolt` | dolt-sql-server | `:3307` | compose net | `hq` DB (beads/sessions/merge/patrol/orch) |
| `postgres` | pg:16 | `:5432` | compose net | audit + outbox + projections |
| `prometheus` | prom v2.55 | `:9090` | compose net | scrapes gt-api/gt-mcp/gt `/metrics` |
| `tempo` | tempo 2.6 | `:4317/4318` | compose net | OTLP trace sink |
| `grafana` | grafana 11 | `:3000` | `127.0.0.1:3000` + traefik | dashboards |

Frontend talks to **gt-api** only. Same origin, no CORS. gt-mcp is for agent
clients (gt-mcp-cli, Claude Code), not the browser.

## gt-api HTTP surface

All `/api/*` routes sit behind the bearer middleware ([auth.rs](../../api/crates/bins/gt-web/src/auth.rs)).
`/health`, `/readyz` and `/metrics` sit **outside** the auth layer (probes).

### Auth

| Var | Effect |
|---|---|
| `GT_WEB_TOKEN=<secret>` | bin starts; routes require `Authorization: Bearer <secret>` |
| `GT_WEB_AUTH=disabled` | dev override; runs open; logs warning |
| neither | bin exits with status 2 |

Every request lands in `events.jsonl` as `web.invoked` / `web.unauthorized`
([audit.rs](../../api/crates/bins/gt-web/src/audit.rs)). Actor tag = `web:<sha256-12hex>`.

### Routes

| Method | Path | Handler | Body / Query | Response |
|---|---|---|---|---|
| GET | `/api/sessions` | [`list_sessions`](../../api/crates/bins/gt-web/src/routes.rs) | `?role=<polecat\|deacon\|...>` (optional) | `Vec<SessionDto>` |
| GET | `/api/beads` | [`list_beads`](../../api/crates/bins/gt-web/src/routes.rs) | `?status=pending\|open\|hooked\|...` (default `pending`) | `Vec<BeadDto>` |
| POST | `/api/nudge` | [`nudge`](../../api/crates/bins/gt-web/src/routes.rs) | `NudgeRequest { session: String }` | `NudgeResponse { accepted: bool }` |
| GET | `/api/stream` | [`stream`](../../api/crates/bins/gt-web/src/routes.rs) | — | SSE of `EventRecord` |
| GET | `/health` | [`health`](../../api/crates/bins/gt-web/src/health.rs) | — | 200 `"ok"` (liveness) |
| GET | `/readyz` | [`readyz`](../../api/crates/bins/gt-web/src/health.rs) | — | 200/503 + JSON probe report |
| GET | `/metrics` | [`metrics`](../../api/crates/bins/gt-web/src/routes.rs) | — | Prometheus text |

That is the **entire** read+write surface today. No `/api/snapshot`, no
`/api/run`, no `/api/mail`, no `/api/issues/*`, no `/api/quota/stream`,
no `/api/git/events`, no `/api/session/attach`. Anything the frontend needs
beyond this list is a **gap** that becomes a bead.

### DTOs ([dto.rs](../../api/crates/bins/gt-web/src/dto.rs))

```ts
// /api/sessions
type SessionDto = {
  id: string;
  rig: string;
  state: "spawned" | "working" | "done" | "killed";
  role: string;          // polecat | deacon | refinery | witness | mayor | sheriff | ...
  crew?: string | null;
};

// /api/beads
type BeadDto = {
  id: string;
  title: string;
  status: string;        // pending | open | hooked | ...
  priority: number;      // 0..4
  assignee?: string | null;
};

// /api/nudge
type NudgeRequest  = { session: string };
type NudgeResponse = { accepted: boolean };
```

### SSE `/api/stream`

Single stream of `EventRecord` ([kernel/gt-audit/src/record.rs](../../api/crates/kernel/gt-audit/src/record.rs)):

```ts
type EventRecord = {
  event_id: string;          // → mirrored into SSE `id:` for Last-Event-ID resume
  correlation_id: string;
  causation_id?: string | null;
  ts: string;                // RFC3339
  type: string;              // event kind, table below
  payload: unknown;          // kind-specific JSON
};
```

Same record the `.events.jsonl` log writes (one shape, log = wire). Slow
subscribers get dropped frames — resync via snapshot endpoints, not by
replaying SSE.

#### Event kinds emitted today

Grouped by domain. Use `type` to route.

| Domain | Kinds |
|---|---|
| agent | `agent.spawned`, `agent.killed`, `agent.heartbeat`, `agent.session_end`, `agent.add`, `agent.remove`, `agent.transition` |
| merge | `merge.submit`, `merge.start`, `merge.started`, `merge.complete`, `merge.merged`, `merge.fail`, `merge.failed`, `merge.ready` |
| patrol | `patrol.register`, `patrol.heartbeat`, `patrol.tick`, `patrol.close`, `patrol.lease_registered`, `patrol.lease_closed`, `patrol.lease_expired` |
| orch | `orch.launch_convoy`, `orch.convoy_created`, `orch.convoy_launched`, `orch.convoy_closed`, `orch.convoy_failed`, `orch.complete_member`, `orch.fail_member`, `orch.member_dispatched`, `orch.member_completed`, `orch.member_failed` |
| quota | `quota.sample`, `quota.probe`, `quota.rotate`, `quota.rotated`, `quota.tokens_sampled`, `quota.usage_probed`, `quota.account_limited`, `quota.blocked`, `quota.block_predicted`, `quota.window_reset` |
| scheduling | `scheduling.enqueue`, `scheduling.mark_dispatched`, `scheduling.dispatched`, `scheduling.dispatch_failed`, `scheduling.dispatch_timeout` |
| rig | `rig.add`, `rig.added`, `rig.adopt`, `rig.adopted`, `rig.remove`, `rig.removed`, `rig.set_prefix`, `rig.prefix_changed`, `rig.set_default_branch`, `rig.default_branch_changed` |
| frontier audit (skip in UI) | `web.invoked`, `web.unauthorized`, `mcp.invoked`, `mcp.unauthorized` |

The `web.*` / `mcp.*` records are observability — the domain does not see them.
Frontend should filter them out unless building an audit view.

## gt-mcp surface (not the browser path)

The browser frontend does **not** call gt-mcp. This is the **agent**
frontier — the single channel agents use to talk to the orchestrator.

**Channel = `gt-mcp`.** Registered in `~/.claude.json`; inside Claude Code
every tool listed below is in your tool list as
`mcp__gt-mcp__<tool_with_underscores>` (the dots in the method name become
underscores: `agent.transition.execute` → `mcp__gt-mcp__agent_transition_execute`).
Resources go through `ReadMcpResourceTool(server="gt-mcp", uri="gt://…")` and
`ListMcpResourcesTool(server="gt-mcp")`. **Agents call those directly** — no
shell, no URL, no container path.

Backend wire-up (operator detail, not agent-visible): HTTP transport,
`127.0.0.1:8765/mcp`, container `gastown-gt-mcp`. External clients
(scripts, automation outside Claude Code) hit the same endpoint via
`gt-mcp-cli`.

### Resources (read-only snapshots)

| URI | Title | Returns |
|---|---|---|
| `gt://agent/sessions` | agent.sessions | active sessions + lifecycle state |
| `gt://scheduling/queue` | scheduling.queue | dispatcher queue depth + in-flight capacity |
| `gt://patrol/leases` | patrol.leases | live lease count + expirations emitted |
| `gt://merge/slots` | merge.slots | merge slots + state machine position |
| `gt://orch/convoys` | orch.convoys | convoys + per-member progress |
| `gt://quota/accounts` | quota.accounts | tracked accounts + predictions |
| `gt://rigs` | rigs | rig catalog (name/prefix/remotes/default branch) |

### Tools (validate + execute pairs)

`agent.*` (add, remove, transition), `merge.*` (submit, complete, fail),
`scheduling.*` (enqueue, mark_dispatched, create_bead), `patrol.*` (register,
heartbeat, close, tick), `orch.*` (launch_convoy, complete_member,
fail_member), `quota.*` (sample, probe, rotate, register, retire),
`rig.*` (add, adopt, remove, set_prefix, set_default_branch).

Per-actor scope via `GT_MCP_SCOPE_CONFIG=/etc/gastown/mcp-scope.toml`.
Actor identity from `GT_MCP_ACTOR`.

## Observability for the frontend

- Prom metrics scraped from gt-api `/metrics`, gt-mcp `/metrics`, gt `:9100/metrics`.
- Traces (OTLP → tempo `:4318`). `OTEL_SERVICE_NAME=gt-api|gt-mcp|gt`.
- Grafana at `https://grafana.gastown.codecsrayo.com` (provisioning under
  [deploy/observability/grafana/](../../../deploy/observability/grafana/)).

Frontend itself should ship traces / RUM only if/when added as a follow-up.

## Gap inventory (what the new frontend needs but Rust does not yet expose)

These are **explicit gaps**, not omissions. Each gap maps to a bead in the
[migration plan](frontend-migration-sveltekit.md) / epic
[hq-fe-svelte](#). Filling a gap is a per-feature decision, never
"automatic". Patterns:

1. **Add HTTP route to gt-web** — when the browser needs it and it is
   read-side of an existing domain.
2. **Wrap existing gt-mcp tool** — same operation already exists; promote to HTTP.
3. **New domain** — concept does not exist in Rust yet (e.g. skills/roles).
4. **n/a** — concept not applicable (e.g. CSRF for bearer SPA).

Avoid: porting the Go endpoint shape verbatim. The new frontend is not bound
to the old API; pick the cleanest contract per feature.

### Read-side gaps

| Need | Status | Bead anchor |
|---|---|---|
| `GET /api/quota/accounts` — snapshot (live/limited, slots, reset_at, tags por sesión, /upgrade pending) | **gap** | hq-fe-api-r.1 |
| `GET /api/quota/rotation` — `waiting_unlock[]` + `recent_rotations[since=]` | **gap** | hq-fe-api-r.2 |
| `GET /api/convoys` — snapshot por estado | **gap** (solo SSE `orch.*`) | hq-fe-api-r.3 |
| `GET /api/merges` — slots snapshot | **gap** | hq-fe-api-r.4 |
| `GET /api/feed?since=` — activity feed con histórico (PG projection) | **gap** | hq-fe-api-r.5 |
| `GET /api/sessions?rig=…` — filtro por rig (hoy solo `?role=`) | **gap** | hq-fe-api-r.6 |
| `GET /api/mayor/status` — ATTACHED / DETACHED + heartbeat | **gap** | hq-fe-api-r.7 |
| `GET /api/whoami` — actor + roles[] + scopes[] (RBAC bootstrap) | **gap** | hq-fe-rbac.4 |
| `GET /api/skills` — catálogo de skills | **gap (new domain)** | hq-fe-skills.2 |
| `GET /api/roles` — catálogo + skills habilitadas por rol | **gap (new domain)** | hq-fe-skills.2 |
| `GET /api/roles/:role/scope` — MCP allow/deny derivado de `mcp-scope.toml` | **gap** | hq-fe-skills.4 |
| `GET /api/patrols` — leases snapshot | **gap** | hq-fe-api-r.* (follow-up) |

### Write-side gaps

| Need | Status | Bead anchor |
|---|---|---|
| Command bus interno en gt-root (validate+execute) | **gap** (lógica está, sin abstracción) | hq-fe-api-w.1 |
| `Idempotency-Key` middleware en gt-web | **gap** | hq-fe-api-w.2 |
| `POST /api/beads` (create) · `PATCH /api/beads/:id` (update) | **gap** (solo gt-mcp tools) | hq-fe-api-w.3 |
| `POST /api/beads/:id/transition` (state machine) | **gap** | hq-fe-api-w.4 |
| `POST /api/beads/:id/comments` | **gap** | hq-fe-api-w.5 |
| `DELETE /api/sessions/:id` (kill via gt-polecat SIGTERM) | **gap** | hq-fe-api-w.6 |
| `POST /api/sessions/:id/restart` | **gap** | hq-fe-api-w.7 |
| `POST /api/sessions/:id/interrupt` (tmux send-keys ESC) | **gap** | hq-fe-api-w.8 |
| `POST /api/convoys` · `pause` · `resume` · `members/:m/fail` (e-stop) | **gap parcial** | hq-fe-api-w.9 |
| `POST /api/quota/accounts/:n/rotate` · `retire` | **gap (HTTP)** (existe MCP) | hq-fe-api-w.10 |
| `POST /api/beads/bulk` + rate-limit | **gap** | hq-fe-api-w.11 |
| `POST /api/roles/:role/skills` (toggle) | **gap (new domain)** | hq-fe-skills.3 |

### Auth gaps

| Need | Status | Bead anchor |
|---|---|---|
| JWT firmado con claims `roles[]` + `scopes[]` (vs bearer plano hoy) | **gap** | hq-fe-rbac.1 |
| `roles.toml` unificado con `mcp-scope.toml` | **gap** | hq-fe-rbac.2 |
| Middleware per-scope (no single bearer) | **gap** | hq-fe-rbac.3 |
| Account login pty driver (`POST /api/quota/accounts/:n/login` + token + cancel) | **gap** | hq-fe-auth.* |
| SSE kinds `quota.login_started` / `login_url_ready` / `login_complete` / `login_failed` | **gap** | hq-fe-auth.3 |

### Terminal / interactive gaps

| Need | Status | Bead anchor |
|---|---|---|
| tmux attach over WebSocket (or MCP tool) — structured stream (not raw PTY) | **gap · diseño abierto** | hq-fe-term.0 (spike) |
| Pop-out terminal window | **gap** | hq-fe-term.* (follow-up) |

### No aplica / fuera de alcance

| Pieza Go viejo | Decisión |
|---|---|
| Mail / inbox | **descartado** — dominio no existe en Rust, no se reimplementa |
| Hooks tab | **diferido** — no en MVP |
| Git events stream / Git log tab | **diferido** — no en MVP |
| Dogs tab | **diferido** — overlap con Sessions filtrado por role |
| CSRF double-submit | **n/a** — bearer en header, sin ambient authority |
| Polecats tab (separado) | **diferido** — overlap con Sessions filtrado |
| Escalations tab | **diferido** — surface via Activity (kind filter) inicialmente |

## Auditoría y observabilidad de la frontier

Cada request `/api/*` (aceptado o rechazado) produce un `web.invoked` /
`web.unauthorized` en `events.jsonl` + PG audit (ver
[audit.rs](../../api/crates/bins/gt-web/src/audit.rs)). Para acciones
destructivas, enriquecer el record con `command` + `target` para que el
Activity feed muestre "brayan killed gg-furiosa · 2s ago" (bead
`hq-fe-rbac.5`).

Actor tag actual = `web:<sha256-12hex>(token)`. Tras JWT (hq-fe-rbac.1),
pasa a `web:<actor>` plano (el JWT identifica al humano).
