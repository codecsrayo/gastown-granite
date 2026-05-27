# 06 — Observabilidad y errores semánticos

## La premisa honesta

**Los logs por sí solos nunca son suficientes para errores semánticos.** Registran lo que
*pasó*, no lo que *debería haber pasado*. Y los bugs duros en un sistema event-driven son
casi siempre **ausencias**: el evento A debió disparar B debió disparar C, y C nunca
ocurrió. Ningún `grep` te grita por un evento que no existe.

La buena noticia: el diseño tiene un **sustrato fuerte** porque el núcleo es
**síncrono y puro**, lo que vuelve la lógica de dominio **determinista respecto al stream
de eventos**. Eso habilita **replay**, que es lo que de verdad caza errores semánticos.

### Regla de determinismo (no negociable — es lo que hace funcionar al replay)

El replay re-corre la lógica pura sobre el log grabado. Para que el estado reconstruido sea
**byte-idéntico** al de producción (gate del Paso 3, [08-getting-started.md](08-getting-started.md)),
el núcleo debe cumplir:

1. **El núcleo nunca lee el reloj de pared ni genera aleatoriedad.** `transition()` y los
   `Command::{validate,execute}` no llaman a `Instant::now()`, `OffsetDateTime::now_utc()`
   ni `rand`. El tiempo y la identidad (`event_id`, `correlation_id`, `ts`) se generan
   **solo en el borde** (productores async) y viajan dentro del `Envelope`; el núcleo los
   **consume**, no los produce. El replay re-alimenta los envelopes grabados tal cual.
2. **Los timeouts y expiraciones son eventos reales, no cálculos del núcleo.** Un tick
   periódico vive en un *productor* async (`expectations.rs`, `witness.rs`): compara los
   timestamps grabados contra el reloj y, si vence, **emite** `DispatchTimeout` / `MergeStuck`
   / `PolecatStale` al bus → se persisten en el log. El replay los **lee del log**; jamás los
   recomputa contra un reloj vivo. Un núcleo que decidiera "expiró" mirando la hora durante el
   replay divergiría del run original.

Corolario práctico: si el gate del Paso 3 falla, el sospechoso #1 es reloj o random filtrado
al núcleo, o un timeout calculado en vez de leído del log.

## Las seis piezas del circuito

### 1. Envelope con causación

```rust
// gt-events/src/envelope.rs
pub struct Envelope<E: EventKind> {
    pub event_id: Ulid,
    pub correlation_id: Ulid,      // workflow originante
    pub causation_id: Option<Ulid>,// evento padre  ← clave
    pub ts: OffsetDateTime,
    pub payload: E,
}
```

Con `causation_id` se puede preguntar "muéstrame todo lo causado por el spawn X" y ver que
la cadena murió en `dispatch` sin producir sesión. Es el audit trail regulatorio aplicado
a eventos: *quién hizo qué, causado por qué*.

### 2. Dead-letter (eventos sin handler o handlers que fallaron)

```rust
// gt-bus/src/deadletter.rs
```

- `publish` con **cero suscriptores** → emite `UnhandledEvent` al audit. Es el bug más
  común: añadiste un evento y olvidaste cablear el handler.
- Handler con `Err` → a canal dead-letter, no logueado-y-descartado.

### 3. State machines explícitas por agregado

En cada `state.rs` se modela el ciclo de vida como enum + función de transición que
**rechaza** movimientos ilegales:

```rust
pub enum SessionState { Spawned, Working, Done, Killed }

impl Session {
    pub fn transition(&mut self, to: SessionState) -> Result<(), InvalidTransition> {
        match (self.state, to) {
            (SessionState::Spawned, SessionState::Working) => { self.state = to; Ok(()) }
            (SessionState::Working, SessionState::Done)     => { self.state = to; Ok(()) }
            (_, _) => Err(InvalidTransition { from: self.state, to }),
        }
    }
}
```

Una transición inválida o faltante se vuelve detectable (y en parte, compilable). Un
`Session` que salta de `Spawned` a `Done` sin pasar por `Working` es un error semántico
que el tipo atrapa.

### 4. Expectations / SLAs como eventos

Generalización del patrón de `gt-patrol`: las ausencias se convierten en eventos de
primera clase, visibles en el feed y replay-ables:

- "esperaba dispatch en 30 s, no llegó" → `DispatchTimeout`
- "esperaba `MergeReady` tras `Started` en 5 min" → `MergeStuck`
- "polecat sin heartbeat en N min" → `PolecatStale` (ya implementado en patrol)

Vive en `<dominio>/src/expectations.rs`.

### 5. `gt-feed/problems.rs` — vista de huecos semánticos

Agrupa: eventos `Unhandled*`, todos los `*Timeout`/`*Stuck`/`*Failed`, y las entradas
dead-letter. Es la vista que mira un humano cuando "algo no anda".

### 6. Replay determinista — `gt-audit/src/replay.rs` + `bins/gt-replay`

La pieza más valiosa, y solo es posible porque el núcleo es síncrono y puro:

```
.events.jsonl (producción)
        │
        ▼
   gt-replay  ──→  re-alimenta la lógica de dominio (sync, pura)
        │
        ▼
   estado final reconstruido  ──→  diff contra estado esperado
                                   o detenerse en la primera divergencia
```

Útil para:
- Reproducir un bug reportado con el log real, sin necesidad de reproducirlo en vivo.
- Verificar que un cambio de lógica no rompe replays históricos (tests en `tests/replay/`).
- Reconciliación: como en banca — tomar el ledger de eventos y reconstruir el estado para
  comparar contra lo que la BD dice que es.

## Tracing + OTEL → Grafana (capa de trazabilidad)

Sobre el sustrato anterior, `gt-telemetry` añade lo estándar, y **es la vía de
trazabilidad hacia Grafana** — no una consulta a una BD de documentos:

- `tracing` con `#[instrument]` por workflow → spans anidados (la cadena de rotación
  aparece como árbol de spans).
- Exporte a OTEL via `tracing-opentelemetry`.
- `correlation_id` / `causation_id` del envelope se propagan como span attributes: la
  cadena causal *es* el trace.

### El stack de Grafana (todo datasource nativo OSS)

| Señal | Backend | Datasource Grafana | Qué responde |
|---|---|---|---|
| **Traces** (cadenas causales) | Tempo | nativo | "muéstrame todo lo causado por el spawn X y dónde murió la cadena" |
| **Métricas** | Prometheus | nativo | tasas, latencias, profundidad de cola, contadores de dead-letter |
| **Log de eventos** (`EventRecord`) | Postgres (`JSONB`) | nativo (SQL) | consulta ad-hoc del audit: filtrar por `actor`, `type`, `correlation_id` |

Deliberadamente **no se usa Mongo**: su datasource en Grafana es Enterprise/community, no
core OSS, y para causación los traces de Tempo son superiores a consultar documentos. El
audit consultable vive en Postgres `JSONB` (ver [04-persistence.md](04-persistence.md)).

## Cobertura honesta

| Caso | Atrapado por |
|---|---|
| Crash / panic | logs + traces |
| Evento procesado dos veces | idempotencia por `event_id` |
| Evento sin handler | dead-letter `UnhandledEvent` |
| Handler que falla silenciosamente | dead-letter de `Err` |
| Cadena causal rota (B nunca disparó C) | `causation_id` + replay |
| Transición de estado ilegal | state machine + `Result` |
| SLA no cumplido (timeout) | expectations → `*Timeout` eventos |
| Divergencia BD ↔ eventos | replay + diff |

Sin esta capa de expectativas, los logs y OTEL atrapan ~70 %. La capa de causación +
dead-letter + state machines + replay sube esa cobertura al rango donde se vuelve útil
para errores semánticos reales, no solo crashes.
