# 07 — Frontend / `gt-web`

> ## ⛔ ALCANCE — `gt-web` es BACKEND, no la UI del navegador
>
> `gt-web` aquí es **solo el lado servidor**: API REST + SSE + comandos write-side
> en Axum.
>
> **La aplicación web del navegador (el dashboard) NO vive en este crate.** Se
> construye aparte en `apps/web/` con SvelteKit + Svelte 5 + Tailwind. El
> contrato HTTP/SSE de `gt-web` es lo único compartido.
>
> - **No portar `internal/web/`** (Go viejo) a Rust ni a estos crates. Está
>   **retirado del despliegue** desde el cutover de compose (commit `c877758e`);
>   sigue en el árbol como referencia histórica, no como spec.
> - El plan completo del frontend nuevo (epics, beads, gaps de API, decisiones de
>   diseño) vive en:
>   - [apps/web/docs/README.md](../../web/docs/README.md) — índice + reglas para agentes
>   - [apps/web/docs/frontend-migration-sveltekit.md](../../web/docs/frontend-migration-sveltekit.md) — alcance + epic plan
>   - [apps/web/docs/frontend-api-surface.md](../../web/docs/frontend-api-surface.md) — contrato real + gaps
>   - [apps/web/docs/frontend-architecture.md](../../web/docs/frontend-architecture.md) — estructura SvelteKit
>   - [apps/web/docs/frontend-features.md](../../web/docs/frontend-features.md) — catálogo de features
>
> - El frontend **NO** asume endpoints inventados. Si una feature necesita algo
>   no documentado en `frontend-api-surface.md`, eso es un **gap explícito** que
>   abre bead en `hq-fe-api-r.*` o `hq-fe-api-w.*` antes de implementarse aquí.
>
> Si un agente empieza a reescribir HTML/CSS/JS dentro de `apps/api`, está fuera
> de alcance — parar y mover ese trabajo al plan SvelteKit en `apps/`.

## Dos naturalezas de dato → dos canales

| Naturaleza | Canal | Tecnología |
|---|---|---|
| **Snapshot** (estado actual) | REST `GET /api/…` | Axum handlers + JSON |
| **Stream** (deltas en vivo) | SSE `EventSource` | broadcast del bus → SSE |

El frontend **nunca** habla con Dolt/Postgres directo. Solo con `gt-web`, que es el
único punto de entrada (composition root del read-side).

## Topología

```
Browser (SvelteKit + Tailwind)
   │
   ├── GET  /api/sessions        → snapshot: sesiones activas             [Dolt]
   ├── GET  /api/beads?rig=…      → snapshot: beads / cola / escalaciones  [Dolt]
   ├── GET  /api/feed?since=<rfc3339>&limit=<n> → snapshot histórico del feed [events.jsonl]
   ├── POST /api/nudge            → emite comando al bus (write-side)
   └── EventSource /api/stream    → SSE: EventRecord en vivo               [bus]
                                     (spawn, nudge, session_death, merge_*)
   ▼
gt-web (bin Axum — composition root)
   ├── gt-agent::SessionQueries     ──► gt-store-dolt
   ├── gt-audit::EventStore          ──► gt-store-pg
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

### Implementación (Paso 7.2, hq-7pdl.2)

- Middleware de autenticación en `bins/gt-web/src/auth.rs`. El binario lee el secreto
  compartido de `GT_WEB_TOKEN` y compara con tiempo constante el header
  `Authorization: Bearer <token>`. Sin token configurado el bin se niega a arrancar; para
  dev existe el override explícito `GT_WEB_AUTH=disabled`. El JWT contra clave pública
  queda como follow-up.
- Cada request autorizada / rechazada produce un record de auditoría (`web.invoked` o
  `web.unauthorized`) que se persiste en el mismo `events.jsonl` que el resto del sistema
  vía `JsonlWebAudit` (`bins/gt-web/src/audit.rs`). El prefijo `web.*` marca observabilidad
  pura: el `replay_gt` salta esos records y el dominio no los ve, igual que con `mcp.*`.
- El identificador que aparece en el audit es un tag derivado del SHA-256 del token
  (`web:<12hex>`); el secreto nunca cae al log.
- El rate-limit por usuario/endpoint queda como follow-up explícito (no bloquea Paso 7).

### Expansión planeada (epic hq-fe-svelte)

Bearer plano migra a **JWT firmado con claims `roles[]` + `scopes[]`**. Tracked en
`hq-fe-rbac.*`:

- `hq-fe-rbac.1` JWT signing en gt-api (HS256/RS256 decidir).
- `hq-fe-rbac.2` `roles.toml` unificado con `mcp-scope.toml` (misma fuente de scopes).
- `hq-fe-rbac.3` Middleware **per-scope** en gt-web (reemplaza el single bearer check).
- `hq-fe-rbac.4` `GET /api/whoami` → `{ actor, roles[], scopes[] }` (bootstrap del
  dashboard).
- `hq-fe-rbac.5` Enriquecer `web.invoked` con `command` + `target` para el audit feed
  ("brayan killed gg-furiosa").

Write-side actual (`POST /api/nudge`) expande a un comando bus completo (tracked en
`hq-fe-api-w.*`); ver gap table en
[apps/web/docs/frontend-api-surface.md](../../web/docs/frontend-api-surface.md).

## Estructura en el árbol

```
bins/gt-web/
└── src/
    ├── main.rs        # arranca Axum, cablea estado compartido + auth + audit
    ├── routes.rs      # endpoints REST (sessions, beads, feed, nudge)
    ├── stream.rs      # bus → broadcast → SSE
    ├── auth.rs        # middleware Bearer + AuthConfig + actor tag
    ├── audit.rs       # WebAuditSink + JsonlWebAudit (web.* frontier-audit)
    └── dto.rs         # SessionDto, BeadDto, FeedEventDto
```

Los DTO son traducción del modelo de dominio a JSON estable: nunca exponer tipos internos
del dominio directo en HTTP (rompe el aislamiento y acopla el cliente a refactors).
