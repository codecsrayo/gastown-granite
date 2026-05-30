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
   ├── GET  /api/quota/rotation?since=<rfc3339>&limit=<n> → cooldown + quota.rotated tail [registry + jsonl]
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

### Implementación (Paso 7.2, hq-7pdl.2 + hq-fe-rbac.1/.2/.4)

- Middleware de autenticación en `bins/gt-web/src/auth.rs`. Tres modos seleccionables al
  arranque (fail-closed, prioridad descendente):
  1. `GT_WEB_JWT_SECRET` → `AuthConfig::Jwt` (HS256, per-actor; hq-fe-rbac.1). El
     issuer/verifier vive en `bins/gt-web/src/jwt.rs` (`JwtIssuer`, `Claims`); decisión
     HS256 vs RS256 documentada ahí — single binary firma + verifica el mismo token, no
     hay verifier externo en el trust boundary, RS256 diferido hasta que entre uno.
  2. `GT_WEB_TOKEN` → `AuthConfig::Bearer` (single shared secret, legacy). Compara con
     tiempo constante; tag de actor derivado de SHA-256(token) (`web:<12hex>`).
  3. `GT_WEB_AUTH=disabled` → `AuthConfig::Open` (sólo dev; warning ruidoso).
  Cualquier otro caso aborta con exit 2.
- En modo JWT el middleware verifica firma + `exp` + `iss`, propaga `sub` como
  `crate::auth::Actor` y adjunta los claims verificados como `AuthClaims` extension al
  request. La razón de fallo (`expired`/`invalid signature`/`malformed`/`issuer
  mismatch`) se distingue en el audit (`web.unauthorized.reason`) pero el cuerpo 401 es
  opaco para no filtrar el motivo a un cliente sondeando.
- Cada request autorizada / rechazada produce un record de auditoría (`web.invoked` o
  `web.unauthorized`) que se persiste en el mismo `events.jsonl` que el resto del sistema
  vía `JsonlWebAudit` (`bins/gt-web/src/audit.rs`). El prefijo `web.*` marca observabilidad
  pura: el `replay_gt` salta esos records y el dominio no los ve, igual que con `mcp.*`.
- El rate-limit por usuario/endpoint queda como follow-up explícito (no bloquea Paso 7).

### RBAC unificada (hq-fe-rbac.2)

- Config canónica: `crates/kernel/gt-rbac::RbacConfig` (TOML/JSON). Una sola fuente de
  verdad para gt-mcp (per-actor MCP tool allow-list) **y** gt-web (per-actor JWT roles
  + scopes). El archivo legacy `deploy/mcp-scope.toml` se mantiene como ubicación
  canónica; gt-mcp lo lee vía `GT_MCP_SCOPE_CONFIG`, gt-web vía `GT_WEB_RBAC_CONFIG` (con
  fallback al mismo `GT_MCP_SCOPE_CONFIG`).
- Schema: `[actors.<id>] allow=[...] validate_only=bool roles=[...]` + `[roles.<name>]
  scopes=[...]`. Back-compat: un archivo sin `roles=` ni `[roles.*]` parsea como antes
  (los bins resuelven a grant vacío y la posture deny-by-default se preserva).
- gt-mcp: `ScopeConfig` es ahora un alias de `RbacConfig`; el puente a `Scope` runtime
  vive en el trait `ResolveScope` (`cfg.resolve(actor) -> Scope`).
- gt-web: `JwtIssuer::with_rbac(Arc<RbacConfig>)` + `sign_for_actor(actor)` consulta el
  config y estampa roles + flattened scopes (dedup por primer-visto) en el token; `sign`
  con valores explícitos se mantiene para tests / paths que no cargan config.

### Expansión planeada (epic hq-fe-svelte)

Estado del epic `hq-fe-rbac`:

- `hq-fe-rbac.1` JWT signing en gt-api — **CLOSED** (HS256, `bins/gt-web/src/jwt.rs`).
- `hq-fe-rbac.2` Config RBAC unificada — **CLOSED** (crate `gt-rbac`).
- `hq-fe-rbac.3` Middleware **per-scope** en gt-web — **CLOSED**
  (`bins/gt-web/src/scope.rs`: `ScopeGuard` por ruta gateado contra `AuthClaims.scopes`;
  Bearer/Open mode grandfathered; emite `web.forbidden` al audit).
- `hq-fe-rbac.4` `GET /api/whoami` → `{ actor, mode, roles[], scopes[] }` — **CLOSED**
  (poblado desde claims JWT cuando `mode=jwt`).
- `hq-fe-rbac.5` Enriquecer `web.invoked` con `command` + `target` para el audit feed
  ("brayan killed gg-furiosa") — **CLOSED**. `WebAuditEvent::Invoked` gana dos campos
  opcionales (`command`, `target`); `scope_middleware` los rellena con `(scope, last id)`
  vía `RouteContext` parqueada en `Response.extensions`, y `auth_middleware` los lee al
  estampar el record final. Cobertura: cualquier ruta con `route_layer(req("..."))` en
  modo JWT; Bearer/Open y rutas sin scope guard (`/api/whoami`) caen al fallback
  method+path. Wire shape estable: campos serializados con
  `skip_serializing_if = "Option::is_none"` para no romper consumidores existentes.

Write-side actual (`POST /api/nudge`) expande a un comando bus completo (tracked en
`hq-fe-api-w.*`); ver gap table en
[apps/web/docs/frontend-api-surface.md](../../web/docs/frontend-api-surface.md).

## Estructura en el árbol

```
bins/gt-web/
└── src/
    ├── main.rs        # arranca Axum, cablea estado compartido + auth + audit
    ├── routes.rs      # endpoints REST (sessions, beads, feed, nudge, whoami)
    ├── stream.rs      # bus → broadcast → SSE
    ├── auth.rs        # AuthConfig (Open/Bearer/Jwt) + middleware + Actor/AuthClaims
    ├── jwt.rs         # HS256 JwtIssuer + Claims (hq-fe-rbac.1, RBAC bind .2)
    ├── audit.rs       # WebAuditSink + JsonlWebAudit (web.* frontier-audit)
    └── dto.rs         # SessionDto, BeadDto, FeedEventDto, WhoamiDto
```

Los DTO son traducción del modelo de dominio a JSON estable: nunca exponer tipos internos
del dominio directo en HTTP (rompe el aislamiento y acopla el cliente a refactors).
