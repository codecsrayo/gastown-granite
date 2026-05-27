# 02 — Árbol del workspace

Estructura objetivo completa. Marcadores de motor de BD entre corchetes `[…]`.

```
gastown-rs/                          # (montado bajo apps/api/ en el repo)
├── Cargo.toml                       # [workspace]
├── rust-toolchain.toml
├── .cargo/
│   └── config.toml                  # linker mold/lld, sccache, dev profile rápido
├── README.md
│
├── crates/
│   │
│   ├── ══ KERNEL ══
│   ├── kernel/
│   │   │   ── Maquinaria de eventos (sync, sin dyn) ──
│   │   ├── gt-events/
│   │   │   └── src/
│   │   │       ├── lib.rs
│   │   │       ├── kind.rs          # trait EventKind { fn kind(&self)->&str }
│   │   │       ├── envelope.rs      # Envelope<E>: event_id, correlation_id, causation_id, ts
│   │   │       ├── ctx.rs           # Ctx (correlation id, CancellationToken)
│   │   │       └── error.rs         # AppError (thiserror)
│   │   ├── gt-bus/                  # Bus<E: EventKind> — fan-out síncrono + relay a canales
│   │   │   └── src/
│   │   │       ├── lib.rs
│   │   │       ├── bus.rs           # publish / subscribe genérico
│   │   │       ├── relay.rs         # handler sync → mpsc → task de I/O
│   │   │       └── deadletter.rs    # UnhandledEvent (0 subs) + canal handler-Err
│   │   ├── gt-plugin/               # ÚNICO sitio con dyn + #[async_trait]
│   │   │   └── src/{lib, plugin}.rs # trait Plugin (watchdogs, sheriffs)
│   │   │
│   │   │   ── Persistencia de eventos ──
│   │   ├── gt-audit/
│   │   │   └── src/
│   │   │       ├── lib.rs
│   │   │       ├── record.rs        # EventRecord (wire/Mongo) + From<Envelope<E>>
│   │   │       ├── store.rs         # trait EventStore
│   │   │       ├── writer.rs        # task que drena mpsc → append (.events.jsonl)
│   │   │       ├── reader.rs        # tail / seek
│   │   │       ├── replay.rs        # re-corre el log por la lógica pura → diff
│   │   │       └── curator.rs       # → .feed.jsonl
│   │   ├── gt-channel/
│   │   │   └── src/{lib, emit, await_event}.rs   # channelevents por archivo (notify)
│   │   │
│   │   │   ── Vocabulario + soporte ──
│   │   ├── gt-beads/
│   │   │   └── src/{lib, bead, fields, repo}.rs  # repo = trait BeadRepository (genérico)
│   │   ├── gt-workspace/
│   │   │   └── src/lib.rs           # town root, paths, FindFromCwd
│   │   ├── gt-telemetry/
│   │   │   └── src/{lib, otel, correlation}.rs   # tracing + OTEL, #[instrument]
│   │   │
│   │   │   ── Adaptadores de BD (async, en los bordes) ──
│   │   ├── gt-store-dolt/
│   │   │   └── src/{lib, conn, beads_repo, commit, wasteland_sync}.rs
│   │   ├── gt-store-pg/
│   │   │   └── src/{lib, pool, quota_repo, outbox}.rs
│   │   └── gt-store-mongo/
│   │       └── src/{lib, client, audit_store, feed_proj}.rs
│   │
│   └── ══ DOMINIOS (enum de eventos owned + actor dueño del estado + state machine) ══
│       │
│       ├── gt-agent/                                                       [Dolt]
│       │   └── src/
│       │       ├── lib.rs
│       │       ├── events.rs        # enum AgentEvent { Spawned, SessionEnd, Killed, … }
│       │       ├── state.rs         # SessionRegistry + enum SessionState + transition()
│       │       ├── actor.rs         # task dueña del estado + enum AgentMsg
│       │       ├── repo.rs          # extiende BeadRepository
│       │       ├── supervisor.rs    # PRODUCER: tmux + heartbeat (async)
│       │       └── handlers.rs      # reactores síncronos
│       │
│       ├── gt-scheduling/                                                  [Dolt]
│       │   └── src/
│       │       ├── lib.rs
│       │       ├── events.rs        # enum SchedEvent { Enqueue, Dispatch, DispatchFailed, DispatchTimeout }
│       │       ├── state.rs         # Queue, CapacityGovernor + state machine de bead
│       │       ├── actor.rs         # dispatcher serializado (dueño del dequeue)
│       │       ├── repo.rs
│       │       ├── dispatcher.rs    # PRODUCER
│       │       ├── expectations.rs  # SLAs como eventos (lease / timeout)
│       │       └── handlers.rs
│       │
│       ├── gt-merge/                                                       [Dolt]
│       │   └── src/
│       │       ├── lib.rs
│       │       ├── events.rs        # enum MergeEvent { Started, Merged, Failed, Ready }
│       │       ├── state.rs         # MergeSlot + state machine (Ready→Merging→Merged|Failed)
│       │       ├── actor.rs
│       │       ├── repo.rs
│       │       ├── refinery.rs      # PRODUCER (await MERGE_READY vía gt-channel)
│       │       └── handlers.rs
│       │
│       ├── gt-patrol/                                                      [Dolt]
│       │   └── src/
│       │       ├── lib.rs
│       │       ├── events.rs        # enum PatrolEvent { PolecatNudged, Escalation* }
│       │       ├── state.rs         # HealthState + transiciones
│       │       ├── actor.rs
│       │       ├── repo.rs
│       │       ├── witness.rs       # PRODUCER (monitor semántico: stale → evento)
│       │       └── handlers.rs
│       │
│       ├── gt-orchestration/                                              [Dolt]
│       │   └── src/
│       │       ├── lib.rs
│       │       ├── events.rs        # enum OrchEvent { ConvoyCreated, Handoff, Delegation }
│       │       ├── state.rs         # Rig, Convoy, Group + state machine
│       │       ├── actor.rs
│       │       ├── repo.rs
│       │       └── mayor.rs · deacon.rs · crew.rs
│       │
│       ├── gt-quota/                                                       [Postgres]
│       │   └── src/
│       │       ├── lib.rs
│       │       ├── events.rs        # enum QuotaEvent { AccountLimited, Rotated, Blocked, Probed }
│       │       ├── state.rs         # Account + AccountQuotaStatus (state machine)
│       │       ├── actor.rs         # serializa la rotación
│       │       ├── repo.rs          # trait AccountRepository
│       │       ├── orchestrator.rs  # cadena SECUENCIAL; soporta --only
│       │       ├── handlers/        # Chain of Responsibility (await en serie)
│       │       │   ├── mod.rs
│       │       │   └── planner.rs · probe.rs · rotation.rs · keychain.rs · cooldown.rs · audit.rs
│       │       └── keychain/        # credenciales (platform-specific)
│       │           ├── mod.rs       # trait Keychain
│       │           └── linux.rs · stub.rs
│       │
│       └── gt-feed/                 # CONSUMIDOR PURO — solo gt-audit              [Mongo]
│           └── src/
│               ├── lib.rs
│               ├── curator.rs       # lee EventRecord, deriva estado (síncrono)
│               ├── problems.rs      # agrupa huecos: unhandled, timeouts, dead-letter
│               └── view.rs          # proyección TUI
│
├── bins/
│   ├── gt/                          # COMPOSITION ROOT
│   │   └── src/
│   │       ├── main.rs              # crea el runtime tokio (uno), cablea todo
│   │       ├── event.rs             # enum GtEvent { Agent(AgentEvent), Merge(MergeEvent), … }
│   │       └── wiring.rs            # actores, drain tasks, suscribe handlers, dead-letter
│   ├── gt-replay/                   # debugging: replay del log + diff esperado/real
│   │   └── src/main.rs
│   ├── gt-web/                      # API + SSE (Axum)
│   │   └── src/{main, routes, stream, dto}.rs
│   ├── gt-proxy-server/             # punto de aplicación: consume gt-quota
│   │   └── src/main.rs
│   └── gt-proxy-client/
│       └── src/main.rs
│
├── tests/
│   ├── contract/                    # matching exhaustivo de eventos entre dominios
│   ├── integration/                 # lifecycle, await-event, merge flow, rotation chain
│   └── replay/                      # logs grabados → estado final esperado
│
└── xtask/                           # build / migración (reemplaza Taskfile.yml)
    └── src/main.rs
```
