# 07 — Frontend / `gt-web`

## Dos naturalezas de dato → dos canales

| Naturaleza | Canal | Tecnología |
|---|---|---|
| **Snapshot** (estado actual) | REST `GET /api/…` | Axum handlers + JSON |
| **Stream** (deltas en vivo) | SSE `EventSource` | broadcast del bus → SSE |

El frontend **nunca** habla con Dolt/Postgres/Mongo directo. Solo con `gt-web`, que es el
único punto de entrada (composition root del read-side).

## Topología

```
Browser (React / Astro + Tailwind)
   │
   ├── GET  /api/sessions        → snapshot: sesiones activas             [Dolt]
   ├── GET  /api/beads?rig=…      → snapshot: beads / cola / escalaciones  [Dolt]
   ├── GET  /api/feed?since=1h    → snapshot inicial del feed              [Mongo]
   ├── POST /api/nudge            → emite comando al bus (write-side)
   └── EventSource /api/stream    → SSE: EventRecord en vivo               [bus]
                                     (spawn, nudge, session_death, merge_*)
   ▼
gt-web (bin Axum — composition root)
   ├── gt-agent::SessionQueries     ──► gt-store-dolt
   ├── gt-audit::EventStore          ──► gt-store-mongo
   └── subscribe(gt-bus)  ──► tokio::broadcast ──► cada conexión SSE
```

## Puente bus → SSE

El `gt-bus` es síncrono in-process; **no se expone directo** al navegador. Se registra
**un** handler que reenvía a un `tokio::sync::broadcast`, y cada conexión SSE se suscribe
a ese broadcast:

```rust
// bins/gt-web/src/stream.rs
pub fn wire_stream(bus: &Bus<GtEvent>) -> broadcast::Sender<EventRecord> {
    let (tx, _) = broadcast::channel(1024);
    let tx2 = tx.clone();
    bus.subscribe("*", move |_ctx, env| {
        let _ = tx2.send(EventRecord::from_envelope(env));
        Ok(())
    });
    tx
}

async fn stream(State(tx): State<broadcast::Sender<EventRecord>>) -> Sse</* … */> {
    let rx = tx.subscribe();
    let s = BroadcastStream::new(rx)
        .filter_map(|r| r.ok())
        .map(|rec| Event::default().json_data(rec).unwrap());
    Sse::new(s).keep_alive(KeepAlive::default())
}
```

## Snapshot endpoints (ejemplo)

```rust
// GET /api/sessions
async fn sessions(State(repo): State<Arc<dyn SessionQueries>>) -> Json<Vec<SessionDto>> {
    Json(repo.active_sessions().await?)
}
```

`SessionQueries` vive en `gt-agent`. Lo implementa `gt-store-dolt`. `gt-web` solo cablea —
no contiene lógica de dominio.

## Flujo del frontend

Para "mostrar las sesiones":

1. `GET /api/sessions` una vez → pinta la tabla.
2. Abre `new EventSource('/api/stream')`.
3. Al llegar `agent.spawned` / `agent.session_death` / `agent.heartbeat`, **parchea** esa
   fila (no re-pide la tabla).

Es exactamente el patrón del `gt feed` TUI, pero el delta llega por SSE en lugar del log
local. Ambos comparten **el mismo `EventRecord`** (ver [03-events.md](03-events.md)).

## SSE, no WebSocket

El stream es **unidireccional** (servidor → navegador, append-only). SSE da reconexión
automática y `Last-Event-ID` gratis, encaja perfecto con un log de eventos. WebSocket solo
sería necesario si el navegador *emitiera* comandos — y eso se resuelve mejor con un
`POST /api/nudge` normal que publica al bus, manteniendo **lectura y escritura separadas**
(CQRS).

## Punto único de entrada → seguridad y auditoría

Como `gt-web` es el único camino al dato:

- Autenticación / IAM se hace **ahí**, no en cada dominio.
- Rate-limit por usuario / por endpoint, **ahí**.
- "Quién consultó qué" se audita **ahí** (otra entrada al `gt-audit` log).

Los dominios y las BD nunca quedan expuestos. Mismo principio que en un API gateway
bancario: la frontera es el único sitio donde se aplica política.

## Estructura en el árbol

```
bins/gt-web/
└── src/
    ├── main.rs        # arranca Axum, cablea estado compartido
    ├── routes.rs      # endpoints REST (sessions, beads, feed, nudge)
    ├── stream.rs      # bus → broadcast → SSE
    └── dto.rs         # SessionDto, BeadDto, FeedEventDto
```

Los DTO son traducción del modelo de dominio a JSON estable: nunca exponer tipos internos
del dominio directo en HTTP (rompe el aislamiento y acopla el cliente a refactors).
