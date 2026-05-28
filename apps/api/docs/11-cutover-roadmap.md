# 11 — Cutover roadmap: Go → Rust (Pasos 8 y 9)

Hoja de ruta para que la API Rust (`apps/api`) **reemplace** el orquestador Go vivo, con
las épicas, dependencias e instrucciones de despacho por agente.

> Estado base (al 2026-05-28, main `bbca70b0`/`46af313a`): el backend Rust boota, persiste a
> Dolt/Postgres, sirve HTTP+SSE+MCP, tiene IAM + telemetría + RealEffects + replay
> determinista. **No es reemplazo de producción todavía** — ver gaps abajo.

## Resumen de los dos pasos

- **Paso 8 (`hq-8iur`)** — hacer el backend Rust *reemplazable* (cutover backend). Cierra los
  gaps duros y entrega la validación shadow + el runbook de flip.
- **Paso 9** — completar el resto Go→Rust (CLI, plugins, wisp/reaper, roles, lifecycle,
  operator signals) para poder **descartar el Go por completo**.

Sin Paso 9 se puede operar en **coexistencia híbrida** (Rust backend + Go CLI/lifecycle).
El flip de Paso 8 NO debe ejecutarse antes de que sus children estén DONE y un humano
apruebe el go/no-go (8.6).

---

## Gaps verificados (por qué existe este roadmap)

1. **Actores no hidratan al boot.** `gt-merge/actor.rs` etc. arrancan en `XxxBoard::default()`;
   el repo Dolt es mirror write-only, el event log es autoritativo pero el boot nunca lo
   re-alimenta a los actores. Restart = estado vivo vacío. → **8.1**
2. **Sessions write-path es Go-only.** `DoltSessions` es solo lectura; la tabla `sessions` la
   puebla `gt sling` (Go) y no existe en prod DBs. `/api/sessions` devuelve `[]`. → **8.2**
3. **Formatos de evento INCOMPATIBLES.** El log Go (`/gt/.events.jsonl`: `{ts, source, type,
   actor, payload, visibility}`, vocabulario `session_start`/`quota_scanned`) **no** coincide
   con el `EventRecord` Rust (`{event_id, correlation_id, causation_id, ts, type, payload}`,
   vocabulario `agent.spawned`/`quota.usage_probed`). Un log Go NO replay-ea en Rust as-is.
   → **8.8** (bloquea el shadow 8.4)
4. **SessionRole/crew no modelado.** `Session{id,rig,state}` no distingue Mayor/Dog/Polecat ni
   atribuye crew. → **8.7** (bloquea 8.2)
5. **74 paquetes Go `internal/` vs ~14 crates Rust.** Sin portar: CLI, gt-plugin (vacío),
   wisp/reaper, roles (Mayor/Witness/Refinery/Deacon/Sheriff), polecat/crew/daemon/tmux
   lifecycle, hooks (GT_HOOK_BEAD), mail, activity log, escalation actions. → **Paso 9**

---

## Paso 8 — cutover backend (`hq-8iur`, P1)

| Bead | Entregable | Pri | Bloqueo |
|---|---|---|---|
| hq-8iur.7 | SessionRole + crew schema (Mayor/Dog/Polecat + crew attribution) | P1 | bloquea .2 |
| hq-8iur.1 | Boot hydration — replay log → actores al boot | P1 | — |
| hq-8iur.2 | Sessions write-path en Rust (poblar tabla Dolt) | P1 | espera .7 |
| hq-8iur.8 | Event-format translation Go→Rust + golden-log portability test | P1 | bloquea .4 |
| hq-8iur.3 | Paridad audit Go↔Rust (mapa gt commands → API) | P2 | — |
| hq-8iur.5 | Ops readiness (`/health`+`/readyz`, graceful shutdown, daemon) | P2 | — |
| hq-8iur.4 | Shadow/parallel-run harness (Rust read-only, diff vs Go) | P2 | espera .8 |
| hq-8iur.6 | [DECISION] cutover runbook + go/no-go flip + rollback | P1 | espera 1-5,7,8 + humano |

Camino crítico: **.7 → .1/.2 → .8 → .4 → .6**. (.3 y .5 en paralelo.)

---

## Paso 9 — completar Go→Rust (descartar Go)

| ID | Epic | Pri | Cubre |
|---|---|---|---|
| hq-hapx | 9.A — `gt` CLI port (`internal/cmd/`) | P1 | gt sling/prime/done/doctor/daemon |
| hq-evks | 9.B — `gt-plugin` system (trait + registry) | P1 | watchdogs/sheriffs |
| hq-t9vt | 9.C — Wisp + Reaper (ephemeral memory + cleanup) | P2 | `internal/wisp`, `internal/reaper` |
| hq-92z9 | 9.D — Role behaviors | P1 | Mayor/Witness/Refinery/Deacon/Sheriff + escalation |
| hq-63az | 9.E — Polecat/Crew/daemon/tmux lifecycle + **hooks (GT_HOOK_BEAD)** | P2 | spawn/heartbeat/restart |
| hq-mysw | 9.F — operator signals | P2 | activity log + escalation action + mail |

Orden Paso 9: **9.B + 9.C** (cero deps) → **9.D** (tras 9.B) → **9.A + 9.E + 9.F** (tras 9.D).

---

## Reglas para todos los agentes

- **Worktree, nunca town root** (auto-revert a main). `git worktree add -b <rama> <ruta> main`.
- **`bd export --all -o /gt/.beads/issues.jsonl` tras CADA mutación bd** — los writes se pierden sin export (back-to-back bd commands).
- **Claim antes de implementar**: marca el bead in_progress + comment. Re-lee el README + `bd list` del epic justo antes de empezar (multi-agente).
- **Cierre**: `cargo build` + `cargo test --workspace` verdes → rama → `git merge --ff-only` a main → push → borra rama. Cierra el bead con el commit SHA. NO PR (project CLAUDE.md: direct-merge).
- **Hotspot común**: `bins/gt/src/root.rs` + `bins/*/src/main.rs`. **Rebase sobre main ANTES de merge**, unión aditiva de conflictos (patrón probado en Paso 6.h/7).
- **Núcleo sync, async solo en bordes. Replay byte-idéntico es gate en cada paso.**

---

## Instrucciones por agente

### Paso 8 — Agente A (hq-8iur.7 → .1 → .2)

```
Trabaja hq-8iur.7, hq-8iur.1, hq-8iur.2 SECUENCIAL, mismo worktree. 8.7 desbloquea 8.2; 8.1 entre medio.
Worktree: git worktree add -b feat/hq-8iur-cutover-A <ruta> main.

8.7 SessionRole + crew (primero):
- gt-agent::Session: + role: SessionRole, + crew: Option<String>
- SessionRole = Mayor | Dog(Witness|Refinery|Deacon|Sheriff) | Polecat. Crew NO es Session (atributo del polecat).
- AgentEvent::Spawned lleva role + crew. DoltSessions::ensure_schema: + role TEXT NOT NULL, crew TEXT NULL.
- /api/sessions DTO + filtro ?role=polecat. MCP resource gt://agent/sessions surface role+crew.
- Enum Dog: enumera del Go (coordina con B/8.3). Gate: spawn Mayor+Polecat+Witness → 3 rows distintos, ?role=polecat=1, replay byte-idéntico.

8.1 boot hydration:
- Al boot, reconstruye estado de actores desde el event log ANTES de servir (merge slots/patrol leases/convoy/quota).
- Approach: leer events.jsonl → fold per-domain (replay_gt) → seed cada actor vía hydrate msg/constructor.
- Gate: siembra estado, mata proceso, reinicia, assert estado idéntico (no vacío) sin eventos nuevos.

8.2 sessions write-path (tras 8.7):
- Projector que escucha AgentEvent en el broadcast → upsert idempotente en Dolt sessions con role+crew.
- Gate: Spawned->Working->Done, /api/sessions lo refleja live + tras restart.

Hotspot: bins/*/main.rs + root.rs (agente C/8.5 también). Rebase antes de merge.
```

### Paso 8 — Agente B (hq-8iur.3 paridad audit)

```
Trabaja hq-8iur.3 (audit paridad Go↔Rust). Solo doc, cero conflicto código.
Worktree: feat/hq-8iur-parity.
Scope: enumera cada gt command/daemon del Go (gt sling/prime/done/doctor, daemon, patrol, refinery, convoy, quota, bead lifecycle) y mapea a su equivalente Rust (HTTP route / MCP tool / RealEffects edge). Fuentes Go: internal/cmd/*.go. Rust: bins/* + domain/*/commands.rs.
Extra: enumera los TIPOS de Dog (witness/refinery/deacon/sheriff) + cómo se identifican en spawn (cmdline/env/path rig) — agente A/8.7 los necesita.
Entregable: tabla COVERED/PARTIAL/MISSING en apps/api/docs/10-go-rust-parity.md, marca MISSING en camino crítico del flip.
```

### Paso 8 — Agente C (hq-8iur.5 ops readiness)

```
Trabaja hq-8iur.5 (operational readiness). Worktree: feat/hq-8iur-ops.
Scope:
- gt-web: /health (liveness) + /readyz (ready solo tras hydration done + Dolt/PG reachable). FUERA del IAM middleware (probes sin token). /metrics también fuera de auth.
- Graceful shutdown: kill -TERM flushea OTLP + drena PG outbox + aborta tasks limpio (hoy varios solo .abort()). Ver spawn_pg_outbox_pipeline.
- Daemon unit: systemd (o gt daemon) con restart policy.
Gate: kill -TERM drena limpio (0 outbox rows perdidas), /readyz cambia correcto en boot.
Coordinación: /readyz depende del flag hydration-done de 8.1 (agente A). Si A no mergeó, expón el hook (AtomicBool) y deja readyz=ready; A lo conecta. Rebase antes de merge (hotspot bins/main.rs con A).
```

### Paso 8 — Agente D (hq-8iur.8 event-format translation)

```
Trabaja hq-8iur.8 (event-format translation Go→Rust + golden-log portability test). Worktree: feat/hq-8iur-portability.
CRITICO: log Go (/gt/.events.jsonl: {ts,source,type,actor,payload,visibility}, vocab session_start/quota_scanned) ≠ EventRecord Rust ({event_id,correlation_id,causation_id,ts,type,payload}, vocab agent.spawned/quota.usage_probed). Un log Go NO replay-ea en Rust as-is.
Scope:
- Decide approach (a) traductor Go→Rust (mapea vocabulario + sintetiza correlation/causation del orden/actor Go) o (b) cutover escribe log Rust-format desde cero sin replay histórico. Documenta.
- Si (a): adapter edge que lee línea Go events.jsonl → emite EventRecord Rust. Enumera vocabulario Go completo (de /gt/.events.jsonl + internal/bus + internal/townlog), mapea cada uno o marca unmappable.
- Golden-log test: captura slice real de /gt/.events.jsonl como fixture → traductor + replay_gt → assert reconstruye sin error + estado coincide con Go (o subset documentado).
Gate: fixture Go replay-ea limpio por el path Rust; divergencias explicadas, no silenciosas. Bloquea 8.4. Coordina con 8.3 (vocabulario) y 8.4 (consume el traductor).
```

### Paso 9 — Agente 9.B (hq-evks gt-plugin, primero)

```
Trabaja hq-evks (gt-plugin trait + registry + watchdogs/sheriffs). Worktree: feat/hq-evks-plugin.
Estado: gt-plugin/src/lib.rs = 7 líneas TODO. ÚNICO sitio del kernel con dyn + #[async_trait].
Scope: trait Plugin { async fn on_event(env: &EventRecord) -> Result<(),AppError> } + name(); PluginRegistry; wire en root.rs (subscribe broadcast, fan-out a plugins en task aparte); port internal/plugin/ Go (recording/scanner/sync); stub Sheriff plugin (9.D lo rellena).
Replay-safe: plugins observan, no mutan dominio. Gate: synthetic event drives chain en orden; errors → dead-letter; replay_gt igual con/sin plugins. Bloquea Sheriff en 9.D.
```

### Paso 9 — Agente 9.D (hq-92z9 roles, tras 9.B)

```
Trabaja hq-92z9 (Mayor/Witness/Refinery/Deacon/Sheriff). Worktree: feat/hq-92z9-roles.
Deps: hq-evks (Sheriff es Plugin) + hq-8iur.7 (SessionRole). Stub si no mergearon.
Scope: una crate por rol siguiendo patrón gt-merge (actor+commands+events+state+repo):
  gt-mayor (orchestration loop), gt-witness (patrol vivo+escalation), gt-refinery (merge gates await MERGE_READY), gt-deacon (shutdown/drain), gt-sheriff (watchdog como Plugin).
Wire bins/gt/root.rs + tools gt-mcp. Gate por rol: test actor lifecycle + replay byte-idéntico. Desbloquea 9.A y 9.E.
```

### Paso 9 — Agente 9.A (hq-hapx CLI, tras 9.D)

```
Trabaja hq-hapx (gt CLI port internal/cmd/). Worktree: feat/hq-hapx-cli.
Deps: hq-92z9 (la mayoría de commands invoca un rol). Empieza por los que solo tocan el backend existente (gt enqueue/rotate/bead) si 9.D no mergeó.
Scope: bins/gt-cli (clap v4). Cada command = thin wrapper: HTTP a gt-web / MCP a gt-mcp / edge I/O local. Coordina con 8.3 (mapa command→endpoint). Críticos primero: gt sling/prime/done/bd/doctor/daemon.
Criterio: skills crew-commit/patrol/reaper/backup funcionan contra el CLI Rust sin cambios. Gate: crew-commit corre end-to-end via CLI Rust; diff vs Go = vacío.
```

### Paso 9 — Agente 9.C (hq-t9vt wisp+reaper, paralelo)

```
Trabaja hq-t9vt (Wisp + Reaper). Worktree: feat/hq-t9vt-wisp. Sin deps.
Scope: crate gt-wisp (port internal/wisp/): WispKind (Heartbeat|Ping|Patrol|GcReport|Recovery|Error|Escalation), TTL por kind, WispRepository + DoltWisp adapter, promotion (wisp→bead). Reaper: bin bins/gt-reaper (scan past-TTL → reap → purge), idempotente. Skill reaper funciona via `gt reaper run`.
Gate: seed wisp heartbeat past TTL → reaper compacta → repeat no duplica. Replay-safe.
```

### Paso 9 — Agente 9.E (hq-63az lifecycle + hooks, tras 9.D)

```
Trabaja hq-63az (Polecat/Crew/daemon/tmux lifecycle + hooks). Worktree: feat/hq-63az-lifecycle.
Deps: hq-8iur.7 (role/crew schema); hq-92z9 (Mayor/Deacon) opcional-stub.
Scope:
- gt-agent o nueva gt-polecat: PolecatLifecycle (spawn real tokio::process::Command reemplaza sleep stub, heartbeat mtime, restart policy con backoff). TmuxAdapter (edge). DaemonSupervisor (vigila N polecats). Crew attribution → Session.crew.
- HOOKS: sistema GT_HOOK_BEAD (sling deferred-spawn inyecta bead en env del polecat al spawn). Porta el comportamiento ARREGLADO (memoria gg-0nb: Go shippeó 340f499a pero droppeaba el Issue). Gate hooks: spawn polecat con hook bead → tmux show-environment GT_HOOK_BEAD lo muestra.
Integración: gt-mayor llama PolecatLifecycle::spawn; RealEffects::sling delega aquí.
Gate: spawn polecat real (no sleep) → heartbeat → kill -9 → restart → trackeado en sessions + AgentEvent log.
```

### Paso 9 — Agente 9.F (hq-mysw operator signals, tras 9.D)

```
Trabaja hq-mysw (activity log + escalation + mail). Worktree: feat/hq-mysw-signals.
Deps: hq-92z9 (escalation se dispara desde Witness/Deacon). Stub el wiring de rol si no mergeó.
Scope (3 piezas):
1. Activity log: port internal/activity como vista gt-feed o proyección PG (read-side SQL-queryable).
2. Escalation action: feed detecta hueco past threshold → emite EscalationRaised (status bead + notify), wired vía Witness/Deacon.
3. Mail/notifications: port internal/mail detrás de Notifier PORT (dominio define, adapter edge). Escalation+quota-block+merge-stuck rutean por ahí. Adapter real SMTP/webhook; fake en test.
Notifier es PORT — mail no se importa en dominios. Gate: hueco *Stuck sintético → escalation bead + notificación por Notifier (fake captura). Documenta qué señales ameritan mail vs feed-only.
```

---

## Mapa de cobertura (qué áreas Go están en Rust)

| Área Go | Rust | Estado | Dónde |
|---|---|---|---|
| convoys | gt-orchestration + DoltOrch | ✅ | hq-bdn8 |
| merge | gt-merge + DoltMerge + refinery.rs | ✅ | hq-bdn8 |
| work (queue) | gt-scheduling + gt-feed | ✅ | Paso 5/6 |
| nudge | gt-web /api/nudge → Heartbeat | ✅ | Paso 6 |
| escalations | gt-feed detecta; acción NO | ⚠️ | 9.F + 9.D |
| activity | gt-feed agrupa; panel NO | ⚠️ | 9.F |
| hooks (GT_HOOK_BEAD) | solo label git-hook | ❌ | 9.E |
| email/mail | nada (mailbox≠email) | ❌ | 9.F |
| CLI | nada | ❌ | 9.A |
| plugins | gt-plugin vacío | ❌ | 9.B |
| wisp/reaper | nada | ❌ | 9.C |
| roles (mayor/dogs) | solo schema 8.7 | ❌ | 9.D |
| polecat/crew/daemon/tmux | supervisor stub | ❌ | 9.E |
