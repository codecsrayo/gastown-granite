# Deployment — los 5 servicios

Definidos en `docker-compose.yml` (proyecto `gastown`). Todos en la red `gastown_default`;
solo `gt-api` (vía traefik) y `gt-mcp` (vía `127.0.0.1:8765`) son alcanzables desde fuera.

| Servicio | Container | Imagen | Puerto | Rol |
|---|---|---|---|---|
| dolt | `gastown-dolt` | `dolthub/dolt-sql-server` | 3307 (interno) | estado de dominio (DB `hq`) |
| postgres | `gastown-pg` | `postgres:16-alpine` | 5432 (interno) | audit + outbox + projections |
| gt-api | `gastown-gt-api` | `apps/api/Dockerfile` (slim) | 8787 → traefik | API read-side + SSE |
| gt | `gastown-gt` | `apps/api/Dockerfile.orchestrator` (runtime full) | — | orquestador + spawn |
| gt-mcp | `gastown-gt-mcp` | slim (mismo que gt-api) | 127.0.0.1:8765 | servidor MCP HTTP |

## dolt

`dolt sql-server` passwordless en `0.0.0.0:3307`, dueño del volume `gastown_dolt-data`. El
entrypoint del image se bypassa para correr `dolt` directo; en cada boot **crea idempotente
el superuser `gastown@%`** (el data dir solo trae `root@localhost`, que rechaza clientes de
red). Conexión: `mysql://gastown@dolt:3307/hq`.

## postgres

`gastown`/`gastown`/`gastown`. Volume `gastown_gt-pgdata`. `gt` y `gt-api`/`gt-mcp` corren
`sqlx::migrate!` al boot → tablas `audit_events`, `outbox_events`, `feed_projections`,
`activity_projections`, `accounts`, `token_usage`, `rigs`. Conexión:
`postgres://gastown:gastown@postgres:5432/gastown`.

## gt-api (gt-web)

Read-side: snapshots `/api/*` + SSE + `/health` + `/readyz` (probes fuera del bearer). Hereda
el router traefik `gastown.codecsrayo.com` → 8787. Imagen slim (sin tmux/claude): solo lee y
sirve, no spawnea.

## gt (composition root)

Imagen runtime full (`gastown-gastown` base: claude + tmux + git + bd + dolt). Entrypoint
`tini -- gt-rs` (bypassa el bootstrap Go del base, que haría `bd init` y crash-loop). Monta
el town root en `/gt`. Corre actores + outbox + daemons + spawn. `GT_POLECAT_CMD` define qué
spawnea (`claude` real; `true` = no-op para pruebas sin gasto).

## gt-mcp

Imagen slim (lleva los 3 binarios). `GT_MCP_TRANSPORT=http`, bind `0.0.0.0:8765`, publicado a
`127.0.0.1:8765/mcp`. Scope por actor desde `/etc/gastown/mcp-scope.toml` (horneado).
Ver [`04-mcp.md`](04-mcp.md).

## Volumes compartidos

- `gastown_dolt-data` → dolt.
- `gastown_gt-pgdata` → postgres.
- `gastown_gt-eventlog` (`/var/lib/gastown/events.jsonl`) → montado en `gt`, `gt-api`, `gt-mcp`
  (event log compartido — los tres appendan/leen). Ver [`02-data-stores.md`](02-data-stores.md).

## Config / secretos

`.env` (gitignored) en el town root: `GT_WEB_TOKEN` (bearer de `/api/*`), `GT_POLECAT_CMD`
(default `true` = no-op seguro).
