# 03 — Modelo de eventos

## Contrato base: `EventKind` (sync, sin dyn)

```rust
// gt-events/src/kind.rs
pub trait EventKind: Send + Sync + 'static {
    fn kind(&self) -> &'static str;   // clave de ruteo del bus
}
```

Nada de `trait Event` global con `Box<dyn Event>`. Un trait object con lifetime
arrastra `#[async_trait]`, bounds `Send`/`Sync` y boxing. Se evita con enums owned.

## Eventos como enums owned, por dominio

Cada dominio define **su** enum, dueño de sus datos:

```rust
// gt-agent/src/events.rs
#[derive(Clone)]
pub enum AgentEvent {
    Spawned { session: String, rig: String },
    SessionEnd { session: String },
    Killed { session: String, reason: String },
}

impl EventKind for AgentEvent {
    fn kind(&self) -> &'static str {
        match self {
            AgentEvent::Spawned { .. }    => "agent.spawned",
            AgentEvent::SessionEnd { .. } => "agent.session_end",
            AgentEvent::Killed { .. }     => "agent.killed",
        }
    }
}
```

Ventajas: matching **exhaustivo verificado por el compilador** (añades una variante y
olvidas manejarla → no compila), cero lifetimes, trivialmente `Send`, cero boxing.

## Enum raíz en el composition root

El único que conoce todos los dominios es el binario, así que el enum unificador vive
en `bins/gt`, **no** en el kernel (no se rompe la regla de dependencias):

```rust
// bins/gt/src/event.rs
pub enum GtEvent {
    Agent(AgentEvent),
    Sched(SchedEvent),
    Merge(MergeEvent),
    Patrol(PatrolEvent),
    Orch(OrchEvent),
    Quota(QuotaEvent),
}
impl EventKind for GtEvent { /* delega al inner */ }

// el bus se instancia como Bus<GtEvent>
```

## Envelope: identidad + causación

Todo evento viaja envuelto. La **causación** (no solo correlación) es lo que permite
reconstruir cadenas rotas — ver [06-observability.md](06-observability.md).

```rust
// gt-events/src/envelope.rs
pub struct Envelope<E: EventKind> {
    pub event_id: Ulid,            // identidad de este evento (idempotencia)
    pub correlation_id: Ulid,      // el workflow originante (toda la cadena)
    pub causation_id: Option<Ulid>,// el evento padre que lo disparó
    pub ts: OffsetDateTime,
    pub payload: E,
}
```

## Bus síncrono y genérico

El bus es `Bus<E: EventKind>`, fan-out **síncrono** (como el Go original), sin async,
sin box. La lógica que reacciona corre en la goroutine/llamada que publica.

```rust
// gt-bus/src/bus.rs (resumen)
impl<E: EventKind> Bus<E> {
    pub fn subscribe(&self, kind: &str, h: impl Fn(&Ctx, &Envelope<E>) -> Result<(), AppError> + 'static);
    pub fn publish(&self, ctx: &Ctx, ev: Envelope<E>) -> Result<(), AppError>;
}
```

Los handlers que necesitan **I/O** no se vuelven async: empujan a un canal y una task
long-lived hace el trabajo (relay):

```
publish(ev) → handler síncrono → tx.try_send(ev) → [task drena mpsc y escribe async]
```

`try_send`, **no** `send().await`: un handler sync no puede `.await` (y `publish` puede
correr dentro de una task async, donde un `blocking_send` clavaría un worker de tokio). La
contrapresión, por tanto, **no se aplica bloqueando al bus** — se aplica en el borde del
relay. Política por canal en [05-queues.md](05-queues.md): los sinks durables (audit) usan
buffer bounded grande y, ante overflow, **spillean a disco + emiten un evento de overflow**
(nunca pérdida silenciosa); los sinks lossy (SSE) descartan frames a propósito.

## Dead-letter (parte del diseño, no extra)

```rust
// gt-bus/src/deadletter.rs
```

- **Cero suscriptores** para un kind: el bug más común (añadiste un evento, olvidaste
  cablear el handler). `publish` lo hace ruidoso: emite `UnhandledEvent` al audit.
- **Handler con `Err`**: va a un canal dead-letter, no se loguea-y-descarta.

## Type-erased solo en el cable

El enum tipado vive **en el bus in-process**. Al persistir (audit en Postgres `JSONB`) se convierte a
un `EventRecord` type-erased (`type: String`, `payload: Map`), que es como funciona el
`.events.jsonl` hoy en Go. `gt-feed` consume ese `EventRecord`, nunca el enum.
