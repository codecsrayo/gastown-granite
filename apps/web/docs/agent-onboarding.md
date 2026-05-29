# Agent onboarding — Gas Town MCP

> **Estado:** WIP · primera sección landed por `hq-mcp-onboard.1`.
> Doc canónica consolidada en `hq-mcp-onboard.9`. Las secciones se rellenan
> conforme cierran los beads `.1`–`.5`.

Esta guía cubre lo que un agente nuevo necesita saber para operar en Gas Town
vía MCP: descubrir tools, leer estado, mutar, y reportar huecos. Si algo no
está aquí, probablemente todavía es un bead abierto en `hq-mcp-onboard`.

---

## 1. Descubrir tools y recursos (`hq-mcp-onboard.1`)

Todo lo que un agente puede *hacer* y *leer* en Gas Town pasa por el MCP server
`gt-mcp` (HTTP en `http://127.0.0.1:8765/mcp`, registrado en Claude Code como
el server name `gt-mcp`). No hay APIs paralelas para agentes — si una operación
no existe vía MCP, **no existe**.

### 1.1 Convención de nombres

Los tools del server `gt-mcp` aparecen en la lista del runtime con el prefijo:

```
mcp__gt-mcp__<dominio>_<accion>_<fase>
```

- **dominio** — actor objetivo (`agent`, `scheduling`, `merge`, `orch`,
  `patrol`, `quota`, `rig`).
- **accion** — comando concreto (`add`, `transition`, `enqueue`,
  `create_bead`, `mark_dispatched`, ...).
- **fase** — `validate` o `execute`. Detalle completo en
  [`hq-mcp-onboard.2`](#).

El nombre canónico en docs y eventos usa puntos (`scheduling.create_bead`); el
runtime lo expone con guiones bajos (`mcp__gt-mcp__scheduling_create_bead_execute`).
La conversión es 1:1 — `.` ↔ `_` y prefijo `mcp__gt-mcp__`. Detalle en
[`hq-mcp-onboard.2`](#).

### 1.2 Escanear el catálogo

Tres vías, según contexto de la sesión:

**(a) Desde dentro de Claude Code** — el runtime ya carga schemas de tools
"hot" al inicio; los demás aparecen como *deferred* con su nombre en
`<system-reminder>`. Para traerlos al alcance:

```
ToolSearch  query="select:mcp__gt-mcp__agent_add_validate,mcp__gt-mcp__agent_add_execute"
ToolSearch  query="quota probe"   max_results=5     # búsqueda por keywords
```

Patrón: filtrar por prefijo `mcp__gt-mcp__` para enumerar todo lo del server.

**(b) Desde la CLI** — `gt-mcp-cli` (Rust, instalado vía `cargo install` desde
`/home/nixos/gt-mcp-cli`):

```bash
gt-mcp-cli tools                # nombres + descripciones
gt-mcp-cli tools --full         # incluye JSON schema de inputs
gt-mcp-cli resources            # lista los snapshot URIs
```

**(c) Vía API MCP cruda** — `tools/list` y `resources/list` en el endpoint
`http://127.0.0.1:8765/mcp` (HTTP streamable). Usado por agentes que no son
Claude Code.

### 1.3 Leer estado: recursos

Los snapshots de dominio se publican como **recursos MCP** (read-only,
versionados por evento). Catálogo actual:

| URI | Qué contiene |
|---|---|
| `gt://agent/sessions` | Sesiones vivas + lifecycle state |
| `gt://scheduling/queue` | Cola del dispatcher + capacidad in-flight |
| `gt://patrol/leases` | Leases vivos + total de expiraciones |
| `gt://merge/slots` | Slots de merge + posición en máquina de estados |
| `gt://orch/convoys` | Convoys + estado per-member |
| `gt://quota/accounts` | Cuentas trackeadas + predicciones |
| `gt://rigs` | Catálogo de rigs (prefix, remotes, default branch) |

**Listar** desde Claude Code:

```
ListMcpResourcesTool                   # todos los servers
ListMcpResourcesTool  server=gt-mcp    # filtra al server gt-mcp
```

**Leer** un snapshot:

```
ReadMcpResourceTool  server=gt-mcp  uri=gt://scheduling/queue
```

CLI equivalente: `gt-mcp-cli read gt://scheduling/queue`.

> **Importante:** los recursos son la **única** fuente canónica de estado para
> agentes. No leas Dolt directo (`docker exec dolt sql`) — eso es escape hatch
> de operador, no flujo de agente. Si el recurso que necesitas no existe, es
> un hueco que abre bead (ver `hq-mcp-onboard.4`).

### 1.4 Mutar estado: tools

Todo cambio observable pasa por un par `<tool>.validate` + `<tool>.execute`:

```
mcp__gt-mcp__scheduling_create_bead_validate   # check-only, no muta
mcp__gt-mcp__scheduling_create_bead_execute    # corre validate internamente, luego muta
```

`validate` es opcional (el execute lo corre primero internamente) pero útil
para previewear errores sin consumir slot. Detalle del patrón y por qué
existe en [`hq-mcp-onboard.2`](#) y [`hq-mcp-onboard.3`](#).

### 1.5 Cuando el tool no existe

Si necesitas una operación y `tools/list` no la tiene, **no inventes camino
alterno** (no `docker exec`, no edición directa de Dolt, no scripts). Eso es
el bead `hq-mcp-onboard.4` (gap discipline workflow) y se cubre allá. Por
ahora, regla corta: abrir bead describiendo la operación faltante + payload
esperado, y bloquear el trabajo dependiente.

---

## Secciones pendientes

- `hq-mcp-onboard.2` — validate → execute pattern + convención `.` ↔ `_`
- `hq-mcp-onboard.3` — why-rationale (audit + scope + invariantes + replay)
- `hq-mcp-onboard.4` — gap discipline workflow
- `hq-mcp-onboard.5` — in-session vs out-of-session split
- `hq-mcp-onboard.6` — memory frontmatter version/status
- `hq-mcp-onboard.7` — tool `help` (índice + URIs + version inline)
- `hq-mcp-onboard.8` — tool `report_gap`
- `hq-mcp-onboard.9` — consolidación + índice + cross-links
- `hq-mcp-onboard.10` — CLAUDE.md global + proyecto apuntan acá
