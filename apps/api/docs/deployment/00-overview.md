# Deployment — visión general del runtime

Cómo se **orquesta el sistema vivo** (no el diseño del código — eso está en
[`../01-architecture.md`](../01-architecture.md)). Aquí: los servicios que corren, cómo se
hablan, dónde vive el estado. Stack desplegado por `docker-compose.yml` (proyecto `gastown`).

## Diagrama maestro

```
                         ┌──────────────────────────────────────────────┐
   browser / SvelteKit ─►│        traefik (proxy :80 / :443)             │
                         │   gastown.codecsrayo.com  ──►  gt-api:8787     │
                         └───────────────────────┬──────────────────────┘
                                                 │
 ┌──────────────────── red compose: gastown_default ─────────────────────────────────────┐
 │                                               ▼                                         │
 │   ┌──────────────┐      ┌──────────────────┐      ┌──────────────────┐                  │
 │   │   gt-api     │      │       gt         │      │     gt-mcp        │ ◄── gt-mcp-cli /  │
 │   │  (gt-web)    │      │ composition root │      │   MCP sobre HTTP  │     Claude Code   │
 │   │   :8787      │      │  + daemons vivos │      │     :8765/mcp     │   127.0.0.1:8765  │
 │   │ API read+SSE │      │  + polecat spawn │      │ tools/resources   │                  │
 │   └──────┬───────┘      └────────┬─────────┘      └────────┬─────────┘                  │
 │          │  cada bin boota su PROPIO composition root; NO comparten memoria.            │
 │          │  Sincronizan por 3 planos de datos compartidos:                              │
 │          ▼                       ▼                         ▼                            │
 │   ┌────────────────────────────────────────────────────────────────────────┐          │
 │   │  event log  ── volume gt-eventlog : /var/lib/gastown/events.jsonl         │         │
 │   └────────────────────────────────────────────────────────────────────────┘          │
 │          │                       │                         │                            │
 │          ▼                       ▼                         ▼                            │
 │   ┌──────────────┐      ┌──────────────────┐                                            │
 │   │    dolt      │      │    postgres      │                                            │
 │   │  :3307  hq   │      │     :5432        │                                            │
 │   │ estado/beads │      │ audit + outbox + │                                            │
 │   │ /sessions    │      │ projections      │                                            │
 │   └──────────────┘      └──────────────────┘                                            │
 │                                                                                         │
 │   gt ──spawn──► tmux ──► claude (polecat)    [GT_POLECAT_CMD, workdir /gt = town root]   │
 └─────────────────────────────────────────────────────────────────────────────────────┘
```

## Cómo se orquesta (en una frase por pieza)

- **dolt / postgres** — los dos motores de persistencia. Dolt guarda el estado de dominio
  (beads, sessions, convoys…); Postgres guarda el audit log canónico, el outbox y las
  proyecciones read-side. Ver [`02-data-stores.md`](02-data-stores.md).
- **gt** — el cerebro: corre los actores de dominio, drena el outbox, y los daemons vivos
  (refinery/witness/deacon/estop/mayor) que **actúan solos** y **spawnean polecats** (agentes
  `claude` en tmux). Ver [`03-orchestration.md`](03-orchestration.md).
- **gt-api** — la cara HTTP read-side (snapshots + SSE) que consume el frontend. Hereda el
  dominio público vía traefik.
- **gt-mcp** — expone el control como herramientas MCP (tools validate/execute + resources)
  para clientes LLM / `gt-mcp-cli`. Ver [`04-mcp.md`](04-mcp.md).

Detalle de cada servicio (imagen, puertos, env): [`01-services.md`](01-services.md).

## Regla clave

Los 3 bins (`gt`, `gt-api`, `gt-mcp`) son **procesos independientes**, cada uno con su propio
composition root en memoria. **No comparten estado por RAM** — se coordinan a través del
**event log** (append compartido), **Dolt** (snapshots de estado) y **Postgres** (audit +
proyecciones). El único **escritor de orquestación con efectos** (spawn, merge) es `gt`.
