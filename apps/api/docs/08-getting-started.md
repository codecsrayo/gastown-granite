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
   evento → replay determinista (regla de `docs/06`). Crate: `crates/domain/gt-patrol`.
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
   `crates/kernel/gt-channel`, `crates/domain/gt-merge`. Gates:
   `refinery_drives_slot_to_merged_then_replay_matches`,
   `refinery_failed_merge_records_failed_event_and_replay_matches`.
3. **`gt-quota`** + `gt-store-pg` (primer Postgres; `keychain` platform-specific).
4. **`gt-orchestration`** (mayor / deacon / crew / convoy).
5. **`gt-web`** (cuando ya hay datos significativos que exponer).
6. **`gt-feed`** + adaptador final para el log.

Cada uno repite la receta: enum owned, actor, repo trait, test con repo in-memory,
adaptador con BD real, los dos tests deben pasar, replay reconstruye.

## Qué NO hacer al principio

- **No empezar por `gt-quota`.** El `keychain` platform-specific consume un día sin
  validar nada arquitectónico.
- **No empezar por `gt-web`.** No hay datos que exponer; queda como pieza ornamental.
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
