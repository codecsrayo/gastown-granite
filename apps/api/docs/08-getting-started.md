# 08 — Por dónde se empieza

Hoja de ruta de implementación. Cada paso es un PR independiente con un **gate de
validación** explícito. El orden no es opcional: respeta el grafo de dependencias y la
regla "valida antes de propagar".

## Principios del orden

1. **Por la espina, no por la BD.** Hasta el Paso 3 inclusive no se toca ninguna base de
   datos. Si la arquitectura aguanta sin BD, aguantará con cualquier BD. Si necesita BD
   para funcionar, está mal — y conviene saberlo antes de invertir en adaptadores.
2. **Smallest valuable slice.** Cada paso entrega algo que corre y se prueba, no piezas
   sueltas esperando integración.
3. **Repo in-memory antes que BD real.** Cada dominio se implementa primero contra un
   repo in-memory; el adaptador de BD llega después y debe pasar los mismos tests.

## Paso 0 — Esqueleto del workspace

**Objetivo:** que el grafo de Cargo refleje y *fuerce* la regla de aislamiento.

**Entregable:**
- `Cargo.toml` raíz con `[workspace] members = […]`.
- `rust-toolchain.toml`, `.cargo/config.toml` (linker `mold`/`lld`, `sccache`,
  `profile.dev` rápido).
- Cada crate de `crates/kernel/*` y `crates/gt-*` con un `lib.rs` vacío y un
  `Cargo.toml` con sus dependencias declaradas según el documento de arquitectura.

**Gate de validación:**
- `cargo build` pasa.
- `cargo tree` confirma que **ningún dominio depende de otro dominio**.
- Intentar añadir `gt-agent` como dependencia de `gt-merge` debe ser visible y rechazable
  en review.

## Paso 1 — La espina: `gt-events` + `gt-bus`

**Objetivo:** validar que el bus síncrono + envelope + dead-letter funcionan antes de
ponerles encima cualquier lógica.

**Entregable:**
- `gt-events`: `trait EventKind`, `struct Envelope<E>`, `AppError`, `Ctx`.
- `gt-bus`: `Bus<E: EventKind>` con `subscribe`/`publish` síncronos, `deadletter` que
  detecta cero suscriptores y handlers con `Err`.
- **Cero I/O, cero async.** ~300 líneas de Rust.

**Gate de validación:**
- Test: publish llega a N suscriptores en orden.
- Test: publish sin suscriptores produce `UnhandledEvent` en el canal dead-letter.
- Test: handler que devuelve `Err` no rompe el fan-out; va a dead-letter.

Si este paso duele, **toda** la arquitectura va a doler. Detenerse aquí y revisar el
diseño antes de continuar.

## Paso 2 — Primer vertical slice: `gt-agent` MVP, sin Dolt

**Objetivo:** validar enums owned + actor + bus + supervisor de I/O, de extremo a
extremo, sin BD.

**Entregable:**
- `events.rs`: `enum AgentEvent` + `impl EventKind`.
- `state.rs`: `SessionRegistry` como struct *owned* + `enum SessionState` con
  `transition()`.
- `actor.rs`: task que recibe `AgentMsg` (`Add` / `Remove` / `Snapshot`) por `mpsc`.
- `supervisor.rs`: spawnea un proceso *fake* (`tokio::process::Command::new("sleep")`),
  escribe heartbeat en archivo, detecta stale por `mtime`, publica `SessionEnd`.
- `repo.rs`: `trait SessionQueries` + **implementación in-memory** (sin Dolt todavía).

**Gate de validación:**
- `cargo test` levanta el actor, spawnea un fake polecat, lo mata, observa el
  `SessionEnd` en un suscriptor de test.
- Cero líneas de SQL escritas hasta aquí.

## Paso 3 — Persistencia de eventos + replay: `gt-audit`

**Objetivo:** establecer la propiedad **determinista** del núcleo puro. Es el seguro de
vida del proyecto para errores semánticos.

**Entregable:**
- `writer.rs`: task que drena `mpsc` → `.events.jsonl` con `fd-lock`.
- `reader.rs`: tail / seek.
- `record.rs`: `EventRecord` + `From<Envelope<E>>`.
- `replay.rs`: re-corre el log por la lógica pura de un dominio.

**Gate de validación:**
- Graba 100 eventos del Paso 2 en `.events.jsonl`.
- Los re-corre por `replay.rs`.
- El estado final del `SessionRegistry` reconstruido es **byte-idéntico** al de la
  corrida en vivo.

Sin este gate no se avanza. Si el replay no es determinista, hay impureza filtrada en el
núcleo — async, **reloj de pared, `rand`, o un timeout calculado en vez de leído del log**
(ver la regla de determinismo en [06-observability.md](06-observability.md)) — y hay que
limpiarla antes de seguir.

## Paso 4 — Primer adaptador real: `gt-beads` + `gt-store-dolt`

**Objetivo:** la BD aparece por primera vez. Validar que el puerto del dominio es
correcto porque la BD es intercambiable.

**Entregable:**
- `gt-beads`: `Bead`, `IssueDep`, `trait BeadRepository`.
- `gt-store-dolt`: `conn.rs` (cliente MySQL-wire al puerto 3307), `beads_repo.rs`
  implementando `BeadRepository`, `commit.rs` con `dolt commit`/`diff`/`rollback`.

**Gate de validación:**
- El mismo test del Paso 2 corre **dos veces**:
  1. Con el repo in-memory.
  2. Con `gt-store-dolt` apuntando a una instancia local de Dolt.
- Ambos pasan. Si uno falla y el otro no, el puerto está mal definido — se corrige antes
  de seguir.

## Paso 5 — Segundo dominio: `gt-scheduling`

**Objetivo:** el primer flujo orquestado real. Valida el CAS-claim y la integración
inter-dominio por bus.

**Entregable:**
- `events.rs`, `state.rs` (`Queue`, `CapacityGovernor`), `actor.rs` (dispatcher
  serializado), `dispatcher.rs`, `expectations.rs` (lease / timeout).
- CAS claim en `gt-store-dolt::beads_repo`.

**Gate de validación:**
- Test end-to-end: enqueue → CAS-claim → spawn vía `gt-agent` → heartbeat → completion.
- Replay del log reconstruye el flujo idéntico.
- Ya hay un mini-orquestador funcional con dos dominios + Dolt + replay.

## Paso 6+ — El resto de los dominios

Mismo patrón aplicado en orden:

1. **`gt-patrol`** ✅ DONE — cierra el lease del Paso 5: el actor recibe
   `Register`/`Heartbeat`/`Close`/`Tick(now_secs, timeout)` desde el borde, el detector
   puro emite `LeaseExpired { bead, worker, priority }`, y el composition root reacciona
   con `BeadRepository::cas_release` + re-encolar. El reloj entra como dato en cada
   evento → replay determinista (regla de `docs/06`). Crate: `crates/domain/orchestration/gt-patrol`.
   Gate test: `orchestrated_flow_with_stale_polecat_recovers_via_patrol`.
2. **`gt-merge` + `gt-channel`** ✅ DONE — introduce el mailbox de archivo *entre
   procesos* para `await MERGE_READY`. `gt-channel` expone `Channel::{open, emit,
   subscribe, ack}` sobre `<root>/<name>/<ulid>.event` con escritura atómica
   (write-then-rename) y watcher `notify` (inotify en Linux, sin polling); el subscriber
   drena los archivos preexistentes y luego reenvía live, deduplicando la ráfaga
   `Create`/`Modify(Name(To))`. `gt-merge` define `MergeEvent { Ready, Started, Merged,
   Failed }`, `MergeSlot` con state machine `Ready → Merging → Merged | Failed`
   (transiciones ilegales = `AppError::InvalidTransition`), `MergeBoard` (dueño único en
   el actor), `MergeState` (reducer de replay) y la `refinery` (productor que traduce
   cada mensaje del canal a `Submit` y ackea **después** de empujar → at-least-once;
   payload corrupto se ackea para no entrar en bucle). Crates:
   `crates/kernel/gt-channel`, `crates/domain/orchestration/gt-merge`. Gates:
   `refinery_drives_slot_to_merged_then_replay_matches`,
   `refinery_failed_merge_records_failed_event_and_replay_matches`.
3. **`gt-quota` + `gt-store-pg`** ✅ DONE — primer Postgres del workspace. Trazabilidad de
   tokens por sesión + **rotación predictiva** (`docs/features/token-tracking-prediction.md`).
   `gt-quota` define `QuotaEvent { TokensSampled, UsageProbed, WindowReset, BlockPredicted,
   AccountLimited, Rotated, Blocked }`, `AccountRegistry` (dueño único en el actor, EWMA del
   rate por cuenta/sesión, idempotencia de predicción por ventana), un **predictor puro**
   (`expectations::predict`: `now`, rate y umbral entran como datos; sin reloj ni `rand`) y
   `cost::cost_units` (normaliza tokens a unidad de coste por modelo — no se suman tokens de
   modelos distintos). El puerto `QuotaRepository` (append-only `token_usage`, sin `UPDATE`
   de contadores) lo implementa `gt-store-pg::PgQuota` con `sqlx` (`query`/`query_as`
   runtime, sin macros → no requiere BD en build-time). Crates: `crates/domain/orchestration/gt-quota`,
   `crates/kernel/gt-store-pg`. Gates: contrato `QuotaRepository` corre 2× (in-memory +
   Postgres real si `GT_PG_URL`); `predictive_rotation_fires_before_account_limited_and_replay_matches`
   (seed de rate conocido → ETA < umbral → **un** `BlockPredicted` por ventana → rotación
   antes de `AccountLimited` → replay reconstruye `QuotaState`);
   `fresh_window_after_reset_allows_another_prediction`. El `keychain` platform-specific
   queda detrás del puerto, pendiente de cablear en el bin.
4. **`gt-orchestration`** ✅ DONE — el dominio del **convoy**: un convoy arrastra un conjunto
   ordenado de beads miembro hasta completarlos, alimentando el siguiente cuando el actual
   termina (el *handoff*) y cerrándose cuando todos están `Done`. `OrchEvent {
   ConvoyCreated, ConvoyLaunched, MemberDispatched, MemberCompleted, MemberFailed,
   ConvoyClosed, ConvoyFailed }`; state machine del convoy (`Staged → Launched → Closed |
   Failed`) y del miembro (`Pending → Active → Done | Failed`), ambas rechazan transiciones
   ilegales con `AppError::InvalidTransition`. `ConvoyBoard` (dueño único en el actor) es
   **secuencial**: a lo sumo un miembro `Active`, el siguiente se alimenta solo tras el
   anterior. El convoy avanza por **hechos** (un miembro terminó), no por reloj → replay
   determinista. `mayor`/`deacon` son productores de borde (traducen hechos del bus a
   `OrchMsg`; lanzan convoys, observan beads miembro cerrando); `crew` ejecuta cada
   `MemberDispatched` que el composition root convierte en `gt sling`. Aislamiento: solo
   depende de `gt-events` (no `gt-scheduling`/`gt-merge`/`gt-beads`). Crate:
   `crates/domain/orchestration/gt-orchestration`. Gates:
   `convoy_drives_members_to_close_then_replay_matches` (3 miembros → handoff secuencial →
   cierre → replay reconstruye `OrchState` byte-idéntico) y
   `convoy_member_failure_halts_then_replay_matches` (fallo de miembro halta el convoy, el
   siguiente nunca se alimenta). Persistir el progreso del convoy en Dolt (marcador `[Dolt]`
   de `docs/02-tree.md`) queda como adaptador follow-up; el dominio entrega puro + replay-able
   primero, como `gt-patrol` y `gt-merge`.
5. **`bins/gt` composition root** ✅ DONE — el primer crate en `bins/` y el único autorizado a
   conocer todos los dominios. Aporta el **unificador** `GtEvent` (suma de
   `Agent/Sched/Patrol/Merge/Quota/Orch`) cuyo `kind()` delega al evento interno — por eso
   el `events.jsonl` mantiene su forma type-erased por dominio y el replay por prefijo de
   los Pasos 2–6 sigue funcionando byte-a-byte. `GtState` agrega un reducer por sub-dominio;
   `replay_gt` reconstruye el sistema completo desde el único log. El root spawnea cada
   actor con su relay, drena los seis en un único `select!` async (un único escritor → orden
   total), y cablea las reacciones cross-dominio que los pasos anteriores difirieron
   explícitamente: `SchedEvent::Dispatched → patrol.register` (Paso 6.a),
   `PatrolEvent::LeaseExpired → repo.cas_release + sched.capacity_freed + sched.enqueue`
   (Paso 6.a), `MergeEvent::Ready → merge.start` y `Merged → repo.upsert(done) +
   capacity_freed` (Paso 6.b), `OrchEvent::MemberDispatched → Effects::sling` (Paso 6.d),
   `QuotaEvent::BlockPredicted | AccountLimited → Effects::rotate` (Paso 6.c). Los efectos
   externos (`gt sling`, rotación) y el reloj entran por los puertos `Effects`/`Clock`
   (inyectados — `main` enchufa los reales, el gate inyecta dobles deterministas). Fallos
   de reacción y eventos sin prefijo conocido caen al dead-letter (entrada del kernel
   `gt-bus::DeadLetterEntry`); contador expuesto en `RootHandle::dead_letters()` — el gate
   exige 0. El `dyn`/`#[async_trait]` sigue confinado a `gt-plugin`: el root usa genéricos
   (`R: BeadRepository + Clone`, `FX: Effects`, `CK: Clock`). El runtime tokio único se
   crea en `main.rs` — los dominios siguen recibiendo handles. Crate: `crates/bins/gt`.
   Gate: `multi_domain_flow_through_root_replays_byte_identical` arrastra
   scheduling+patrol+agent+orch a través de un único root + un único log (`enqueue →
   dispatched → lease registered → tick stale → expired → reclaim → re-dispatched →
   completion + convoy launch → handoff → close`), y verifica que `GtState` reconstruido
   por `replay_gt` es **byte-idéntico** al replay por prefijo dominio a dominio (preserva
   el gate del Paso 3 sin perder la unificación).
6. **`gt-web`** ✅ DONE — `bins/gt-web` (Axum backend, lado lectura). El crate cablea
   `gt-agent::SessionQueries`, `gt-beads::BeadRepository` y el broadcast del root en cuatro
   endpoints HTTP — exactamente las dos naturalezas de dato del `docs/07-frontend.md`:
   snapshot (`GET /api/sessions`, `GET /api/beads?status=…`) y stream (`GET /api/stream`
   como SSE), más el comando de escritura `POST /api/nudge` que publica
   `AgentEvent::Heartbeat` en el relay del agente (CQRS: lectura/escritura separadas, el
   navegador nunca habla con Dolt/Postgres directo). El puente bus→broadcast→SSE vive
   dentro del root (Paso 6.e extendido): el reactor que ya es **el único escritor** del log
   también `tx.send(rec)` a un `tokio::sync::broadcast<EventRecord>`; los SSE consumidores
   llaman `RootHandle::subscribe_events()` y reciben byte-idéntico al log (regla "shared
   `EventRecord`"). Aislamiento: `gt-web` depende de `bins/gt`, `gt-agent`, `gt-beads`,
   `gt-audit` y `gt-events`; no toca ningún otro dominio. Genérico sobre `R: BeadRepository`
   y `SQ: SessionQueries` — la prod plug-uea adaptadores Dolt/PG, los tests in-memory; el
   DTO traduce a JSON estable (`SessionDto`, `BeadDto`) sin filtrar tipos internos. Para
   ese acople el puerto `SessionQueries` migró a RPITIT con `+ Send` (mismo estilo que
   `BeadRepository`), porque `axum` exige futuros `Send`. Crate: `crates/bins/gt-web`.
   Gates: `snapshot_endpoints_serve_dto_rows` (REST + 400 ante `status` desconocido),
   `sse_stream_delivers_event_driven_through_root` (evento empujado por el relay del root
   aparece como frame `agent.spawned` en el SSE) y `nudge_emits_heartbeat_visible_via_sse`
   (CQRS end-to-end: `POST /api/nudge` → log → broadcast → SSE → cliente). **Alcance
   explícito**: este crate es BACKEND only — la UI del navegador (`internal/web/`,
   `dashboard.js`, etc.) NO se migra; ese trabajo vive en `apps/town` bajo SvelteKit.
7. **`gt-feed`** + adaptador final para el log.

Cada uno repite la receta: enum owned, actor, repo trait, test con repo in-memory,
adaptador con BD real, los dos tests deben pasar, replay reconstruye.

## Qué NO hacer al principio

- **No empezar por `gt-quota`.** El `keychain` platform-specific consume un día sin
  validar nada arquitectónico.
- **No empezar por `gt-web`.** No hay datos que exponer; queda como pieza ornamental.
  *(Histórico — los Pasos 6.a–6.f ya entregaron los datos suficientes y `gt-web` aterrizó.)*
- **No portear el schema completo de Dolt antes del Paso 2.** La forma del repo va a
  cambiar cuando se use; congelar especulativamente cuesta caro.
- **No saltarse el gate del Paso 3.** El replay determinista es lo que justifica el núcleo
  síncrono. Sin él, la decisión arquitectónica más cara queda sin validar.

## Resumen visual

```
Paso 0  ── esqueleto compila ───────────────────────  sin código real
Paso 1  ── espina (events + bus) ────────────────────  sin I/O, sin async
Paso 2  ── primer slice (gt-agent) ──────────────────  sin BD
Paso 3  ── audit + replay ───────────────────────────  determinismo probado
Paso 4  ── primera BD (Dolt + gt-beads) ─────────────  puerto validado
Paso 5  ── segundo dominio (scheduling) ─────────────  orquestación real
Paso 6+ ── el resto, uno por uno ────────────────────  patrón repetido
```

Cada flecha hacia abajo solo se cruza con el gate anterior verde.
