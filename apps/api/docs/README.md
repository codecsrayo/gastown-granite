# gastown-rs — Documentación de implementación

Migración del orquestador de agentes **Gas Town** (Go) a una API/runtime en **Rust**,
con arquitectura event-driven, dominios aislados y persistencia híbrida.

Esta carpeta describe **cómo debería quedar la implementación**: las fronteras, los
contratos y las decisiones de diseño que el código debe respetar. No es un tutorial;
es la especificación de referencia.

---

> ## ⚠️ AVISO PARA AGENTES — leer antes de tocar gastown-rs
>
> **Hay varios agentes trabajando en este repo.** Esta carpeta es la **fuente de verdad**
> del diseño de la migración a Rust. Reglas para no perder ni pisar trabajo:
>
> 1. **Esto es la spec, no permiso para borrar el Go.** El **gastown Go vivo es producción**
>    y la fuente de verdad operativa; no se toca ni se reemplaza pieza por pieza sin un plan
>    de cutover acordado. La migración Rust es greenfield en paralelo.
> 2. **Reclama antes de implementar.** Antes de empezar un crate/paso, márcalo ocupado
>    (bead en estado busy) y anótalo en la **tabla de estado** de abajo. Al terminar, cierra
>    el bead y actualiza la tabla. Así nadie duplica un dominio o adaptador.
> 3. **Respeta el orden de [08-getting-started.md](08-getting-started.md).** Los pasos tienen
>    dependencia y un **gate** cada uno; no se cruza al siguiente sin el anterior en verde.
>    En especial: **no saltarse el gate del Paso 3** (replay determinista).
> 4. **La spec manda sobre el código.** Si el código diverge de estos docs, o es bug del
>    código o hay que actualizar el doc **con acuerdo** — no dejar que diverjan en silencio.
> 5. **Rama aparte → merge a main → borra la rama.** Nunca directo sobre main (el town root
>    revierte). Usa worktree.
> 6. **Decisiones ya tomadas (no re-litigar sin acuerdo):** núcleo **sync** (async solo en
>    bordes); `Command` sync; persistencia **2 motores** (Dolt + Postgres, audit en `JSONB`,
>    **sin Mongo**); trazabilidad Grafana vía **OTEL→Tempo + Prometheus + Postgres**;
>    `dyn`/`#[async_trait]` confinados a `gt-plugin`.
> 7. **La UI del navegador NO se migra aquí.** `gt-web` es solo backend (API+SSE). **No
>    portar `internal/web/`** (dashboard.js/convoy.html/etc.) a Rust ni a `apps/api`. El
>    frontend va en pista separada con SvelteKit
>    ([plan](../../town/docs/frontend-migration-sveltekit.md)); aquí solo se expone el
>    contrato que consume. Ver [07-frontend.md](07-frontend.md).
>
> ### Tabla de estado (ver pasos en [08-getting-started.md](08-getting-started.md))
>
> | Paso | Entregable | Estado | Agente | Bead |
> |---|---|---|---|---|
> | 0 | esqueleto del workspace | DONE | — | — |
> | 1 | espina `gt-events` + `gt-bus` | DONE | — | — |
> | 2 | slice `gt-agent` (sin BD) | DONE | — | — |
> | 3 | `gt-audit` + replay (gate determinismo) | DONE | — | — |
> | 4 | `gt-beads` + `gt-store-dolt` | DONE | — | — |
> | 5 | `gt-scheduling` | DONE | — | hq-mc72 |
> | 6.a | `gt-patrol` (lease-expired → re-enqueue, cierra el lease del Paso 5) | DONE | — | hq-mc72 |
> | 6.b | `gt-merge` + `gt-channel` (refinery await MERGE_READY) | DONE | — | hq-mc72.2 |
> | 6.c | `gt-quota` + `gt-store-pg` (primer Postgres, rotación predictiva) | DONE | — | hq-mc72.3 |
> | 6.d | `gt-orchestration` (convoy: handoff secuencial + replay) | DONE | — | hq-mc72.4 |
> | 6.e | `bins/gt` composition root (GtEvent unifier, actores/relays/dead-letter) | DONE | — | hq-mc72.6 |
> | 6.f | `gt-web` (API+SSE backend: snapshot + bus->broadcast stream) | DONE | — | hq-mc72.7 |
> | 6.f.1 | `gt-mcp` vertical slice (Command retrofit on gt-agent + stdio JSON-RPC, scope auth, audit) | DONE | — | hq-b6pi |
> | 6.f.2 | `gt-mcp` swap to official `rmcp` SDK (schemars-derived schemas, `#[tool_router]`) | DONE | — | hq-c3hb |
> | 6.g | `gt-feed` (consumidor puro: Curator/Problems/View, replay byte-idéntico, wired en `GtState.feed` via `replay_gt`) | DONE | — | hq-mc72.8 |
> | 6.f.13 | v1 operational: persistence wired in bins (DoltBeads + PgAudit opt-in via env) | DONE | — | hq-j9ou |
> | 7.1 | `RealEffects` (gt sling subprocess + QuotaCommand::Rotate chain wired en `bins/gt` + `bins/gt-web`) | DONE | — | hq-7pdl.1 |
> | 7.2 | `gt-web` bearer-token IAM middleware + `web.*` frontier-audit en events.jsonl | DONE | — | hq-7pdl.2 |
> | 7.a | `gt-telemetry` (OTEL→Tempo + Prometheus exporters wired in `gt`/`gt-web`/`gt-mcp`, `/metrics` route in `gt-web`) | DONE | — | hq-0bko.1 |
> | 7.b | `gt-quota::keychain` (port + `InMemoryKeychain` + Linux Secret-Service adapter; rotation flips live pointer at the edge) | DONE | — | hq-0bko.2 |
> | 7.c | `gt-quota::probe` (parses real `anthropic-ratelimit-*` headers → `ProbeWindow`, idempotent under retry) | DONE | — | hq-0bko.3 |
> | 6.h-A | `gt-store-dolt::DoltSessions` (`SessionQueries` adapter wired en `gt-web` + `gt-mcp` via `GT_DOLT_URL`) | DONE | — | hq-u955 |
> | 6.h-B | domain-state Dolt persistence — `MergeRepository`/`PatrolRepository`/`OrchRepository` ports + `DoltMerge`/`DoltPatrol`/`DoltOrch` adapters (`merge_slots`/`patrol_leases`/`convoys`+`convoy_members`), wired en `gt`/`gt-web`/`gt-mcp` behind `GT_DOLT_URL` fallback to in-memory | DONE | — | hq-bdn8 |
> | 6.h-C | audit outbox + feed projections (`outbox_events` + `feed_projections`; entity+outbox single-TX in `PgOutboxWriter`, `PgOutboxDrain` fans to `audit_events`+projections, wired en `bins/gt`) | DONE | — | hq-7owq |
> | 6.h+ | resto de `gt-mcp` (otros dominios), adaptadores edge real adicionales | PLANEADO | — | — |
>
> Estado global: **Paso 7 (v1 operational + observability + real quota: hq-7pdl + hq-j9ou + hq-0bko) DONE**; **Paso 6.h A/B/C (Dolt read-side + domain-state + audit outbox: hq-u955 + hq-bdn8 + hq-7owq) DONE** (al 2026-05-28). Mantén esta tabla viva.

---

## Índice

| Doc | Contenido |
|---|---|
| [01-architecture.md](01-architecture.md) | Capas (kernel / dominios), regla de dependencias, ports & adapters, modelo de actores, async-en-los-bordes |
| [02-tree.md](02-tree.md) | Árbol completo del workspace |
| [03-events.md](03-events.md) | Modelo de eventos: enums owned, envelope con causación, bus síncrono, dead-letter |
| [04-persistence.md](04-persistence.md) | Persistencia híbrida Dolt / Postgres (audit en `JSONB`); puertos y adaptadores; outbox; trazabilidad Grafana vía OTEL/Tempo |
| [05-queues.md](05-queues.md) | Taxonomía de colas, cola de trabajo sobre Dolt, claim por CAS, Dolt vs InnoDB, backpressure |
| [06-observability.md](06-observability.md) | Seguimiento de errores semánticos: causación, state machines, expectations, replay determinista |
| [07-frontend.md](07-frontend.md) | `gt-web`: API + SSE, snapshot vs stream |
| [08-getting-started.md](08-getting-started.md) | Hoja de ruta de implementación: pasos, entregables y gates de validación |
| [09-llm-integration.md](09-llm-integration.md) | Integración con modelos: `gt-mcp`, patrón `Command { validate, execute }`, scopes |

### Features

| Doc | Contenido |
|---|---|
| [features/token-tracking-prediction.md](features/token-tracking-prediction.md) | `gt-quota`: trazabilidad de tokens por sesión, promedio (EWMA), predicción de ETA-al-bloqueo → rotación predictiva |

## Alcance y objetivo

La ventaja de esta migración **no es rendimiento**. gastown es un orquestador
I/O-bound (spawnea procesos, espera APIs de LLM, vigila archivos); el cuello de
botella nunca es la CPU del orquestador, y Go encaja ese workload de forma natural.

El valor real es:

1. **Eliminar clases de bugs** (nil-pointer / SIGSEGV de Dolt, fragilidad de
   subprocesos `bd`) vía `Option`/`Result` y un cliente nativo.
2. **Fronteras forzadas por el compilador** (aislamiento de dominios, enums
   exhaustivos de eventos) que en Go solo existirían por disciplina.
3. **Maestría en Rust** sobre un sistema real y complejo.

Decisión consciente a sostener: gastown es un proyecto vivo; reescribir persigue
un blanco móvil. El 80 % de la ganancia *arquitectónica* (aislamiento, ports/adapters,
CQRS) es independiente del lenguaje.

## Principios no negociables

- **Los dominios dependen solo del kernel.** Nunca un dominio importa otro.
- **Las BD nunca se exponen.** Los dominios definen traits de repositorio; los
  adaptadores `gt-store-*` los implementan (dependencia invertida).
- **Async en los bordes, síncrono en el núcleo.** La lógica pura no es async.
- **Estado mutable dentro de un actor**, coordinado por canales — no `Arc<Mutex>`
  repartido.
- **Eventos como enums owned**, no trait objects con lifetime.
- **Dolt es la fuente de verdad de los beads** y de la federación Wasteland.