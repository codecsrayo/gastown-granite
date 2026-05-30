# 14 — Taxonomía de beads, epics y dependencias

Disciplina obligatoria al crear/cerrar **beads** y **epics** para que el
ecosistema sea consultable como **grafo de dependencias**, no como lista plana.

Objetivo: una pregunta como *"¿qué falta de la API?"* o *"¿qué beads dependen
de `gt-mcp`?"* se resuelve con **una query al MCP**, no con grep sobre títulos.

---

## 1. Por qué no alcanza el slug

Hoy el id `hq-fe-api-w.1` es legible pero **opaco al filtro**: esconde que el
bead también toca `gt-mcp`, `gt-root` y `apps/web`. Un grep por `api` pierde
beads cuyo slug no contiene la palabra. El slug solo puede expresar un dominio
primario.

La solución es **anotar al crear** y dejar que el grafo lo resuelva.

---

## 2. Campos obligatorios al crear bead/epic

Toda creación pasa por `mcp__gt-mcp__issues_create_execute`
(ver [09-llm-integration.md](09-llm-integration.md)). Política:
nunca `docker exec dolt sql`, nunca `bd` directo desde un agente
(escape hatch operador-only). El payload **debe** incluir:

| Campo | Tipo | Cardinal | Significado |
|---|---|---|---|
| `domain[]` | enum cerrado (§3) | 1..N | dominios funcionales que el bead afecta |
| `surface[]` | crate/path | 0..N | impacto físico — crates o rutas tocadas |
| `depends_on[]` | bead-id | 0..N | bloqueado por estos beads (edge dirigido) |
| `parent_epic` | bead-id | 0..1 | epic raíz; obligatorio en tasks, vacío en epics raíz |
| `role_scope` | enum roles (§4) | 0..1 | rol responsable; valida pertenencia a `domain[]` |

`domain[]` y `surface[]` no son sinónimos:
- `domain[]` es **semántico** (qué área del producto cambia).
- `surface[]` es **físico** (qué crate compila distinto). Útil para detectar
  hotspots de conflicto (e.g. `gt-mcp/service.rs` por
  [[project_gt_mcp_epic_parallel]]).

---

## 3. Taxonomía cerrada de dominios

Anclada a la estructura real de `apps/api/crates/` para no inventar nombres.
**Lista cerrada**: agregar un dominio nuevo requiere `meta.report_gap` con
`hq-gap-domain-<slug>` y aprobación operador.

### 3.1 Kernel (`apps/api/crates/kernel/`)
| `domain` | Crates |
|---|---|
| `kernel.events` | `gt-events` |
| `kernel.bus` | `gt-bus` |
| `kernel.audit` | `gt-audit` |
| `kernel.telemetry` | `gt-telemetry` |
| `kernel.plugin` | `gt-plugin` |
| `kernel.channel` | `gt-channel` |
| `kernel.root` | `gt-root` (CommandBus, hydration) |

### 3.2 Lifecycle (`domain/lifecycle/`)
| `domain` | Crates |
|---|---|
| `lifecycle.agent` | `gt-agent` |
| `lifecycle.polecat` | `gt-polecat` |

### 3.3 Orchestration (`domain/orchestration/`)
| `domain` | Crates |
|---|---|
| `orch.scheduling` | `gt-scheduling` |
| `orch.patrol` | `gt-patrol` |
| `orch.merge` | `gt-merge` |
| `orch.quota` | `gt-quota` |
| `orch.convoy` | `gt-orchestration` |

### 3.4 Platform (`domain/platform/`)
| `domain` | Crates |
|---|---|
| `platform.feed` | `gt-feed` |
| `platform.notify` | `gt-notify` |
| `platform.rig` | `gt-rig` |
| `platform.wisp` | `gt-wisp` |

### 3.5 Roles (`domain/roles/`)
Cada rol es un dominio en sí mismo + valida `role_scope` (§4):

| `domain` | Crate | Dominios cruzados permitidos |
|---|---|---|
| `role.sheriff` | `gt-sheriff` | `orch.merge`, `kernel.plugin` |
| `role.deacon` | `gt-deacon` | `orch.scheduling`, `orch.patrol` |
| `role.refinery` | `gt-refinery` | `orch.merge`, `orch.quota` |
| `role.witness` | `gt-witness` | `kernel.telemetry`, `kernel.audit` |
| `role.mayor` | `gt-mayor` | `lifecycle.agent`, `lifecycle.polecat` |

### 3.6 Bins y edge (`apps/api/crates/bins/`)
| `domain` | Crates |
|---|---|
| `bin.gt` | `gt` (composition root daemon) |
| `bin.gt-web` | `gt-web` (HTTP/SSE) |
| `bin.gt-mcp` | `gt-mcp` (MCP server) |
| `bin.gt-mcp-cli` | externo `/home/nixos/gt-mcp-cli` |

### 3.7 Stores (`domain/.../store-*` y `gt-store-*`)
| `domain` | Crates |
|---|---|
| `store.dolt` | `gt-store-dolt` |
| `store.pg` | `gt-store-pg` |
| `store.beads` | `gt-beads` (port) |

### 3.8 Frontend (`apps/web/`)
| `domain` | Surface |
|---|---|
| `fe.web` | `apps/web/src/**` |
| `fe.docs` | `apps/web/docs/**` |

### 3.9 Deploy / docs
| `domain` | Surface |
|---|---|
| `deploy.compose` | `compose.yml`, `apps/api/docs/deployment/**` |
| `deploy.dolt` | volúmenes Dolt, replicación |
| `docs.spec` | `apps/api/docs/**` (esta carpeta) |

---

## 4. `role_scope` — un bead, un rol responsable

Los **roles** (sheriff/deacon/refinery/witness/mayor — ver
[[project_hq_92z9_roles]]) también se apegan a la taxonomía:

1. Si `role_scope` está set, **todo** dominio en `domain[]` debe pertenecer
   al cruce permitido del rol (§3.5) **o** estar en `bin.*`/`store.*`/`kernel.*`
   (capas que cualquier rol puede tocar de forma controlada).
2. Si un bead toca un dominio fuera del cruce permitido del rol, **se parte
   en dos**: una mitad bajo el rol, otra como sub-bead sin `role_scope`
   coordinado por el epic.
3. Un rol que necesita expandirse a un dominio nuevo abre primero un
   `hq-role-<rol>-scope-<dominio>` (epic) — no se relaja en silencio.

Regla de oro: un bead `role_scope: sheriff` con `domain: [orch.quota]` es
**inválido** — `orch.quota` no está en su cruce. Refinery se encarga.

---

## 5. Edges: `depends_on[]` vs `parent_epic`

Dos relaciones distintas, no mezclar:

- **`parent_epic`** = jerarquía (epic → tasks). Árbol. Una sola.
- **`depends_on[]`** = orden de despacho (este bead no puede empezar
  hasta que esos cierren). Grafo dirigido acíclico. Varias.

Reglas:
1. Un task **siempre** tiene `parent_epic`.
2. `depends_on[]` puede cruzar epics (e.g. `hq-fe-api-w.2` puede depender
   de `hq-mcp-issues.5`). El grafo es global, los epics son agrupación.
3. Cierre de epic = todos los children cerrados **y** sin `depends_on[]`
   pendientes apuntando hacia adentro.
4. Detección de ciclos en `validate` del `issues_create` — fail rápido.

---

## 6. Consistencia epic ↔ children

Al cerrar un epic, su `domain[]` debe ser **superconjunto** de la unión de
`domain[]` de sus children. Concretamente:

```text
epic.domain[] ⊇ ⋃ child.domain[] for child in epic.children
```

Si un child anota un dominio que el epic no declara, el cierre **falla**
con `EpicDomainDrift`. Forma de resolverlo: extender `epic.domain[]`
explícitamente (lo cual deja audit-trail de que el alcance cambió).

---

## 7. Cómo se consulta el grafo

Tres resources MCP expuestos por `gt-mcp`:

| Resource | Devuelve |
|---|---|
| `gt://graph/domain/<d>` | beads + epics con `<d>` en `domain[]`, abiertos y cerrados, con sus `depends_on[]` resueltos a 1 nivel |
| `gt://graph/depends_on/<bead>` | subgrafo transitivo bloqueado por `<bead>` (hacia adelante) |
| `gt://graph/blocks/<bead>` | subgrafo transitivo que bloquea a `<bead>` (hacia atrás) |
| `gt://graph/role/<rol>` | todos los beads con `role_scope = <rol>` agrupados por epic |
| `gt://graph/surface/<crate>` | beads que tocan ese crate — hotspot detector |

Casos de uso:
- *"¿Qué falta de api?"* → `gt://graph/domain/bin.gt-web` ∪
  `domain/bin.gt-mcp` filtrado por `status != closed`.
- *"¿Refinery está ocupado en qué?"* → `gt://graph/role/refinery` con
  filtro `status = busy|ready`.
- *"¿Qué se desbloquea si cierro `hq-fe-api-w.1`?"* →
  `gt://graph/depends_on/hq-fe-api-w.1`.

---

## 8. Migración de beads históricos

Beads pre-directiva no tienen `domain[]`. Política:

1. **No backfill masivo**. Anotar al **tocar** (estilo strangler):
   cualquier bead que se reabra/edite recibe los campos del §2.
2. Epics raíz vivos (`hq-fe-svelte`, `hq-fe-api-w`, `hq-mcp-issues`,
   `hq-oap5`, etc.) sí se backfillan a mano — son pocos y guían el grafo.
3. Beads cerrados sin `domain[]` quedan como están; el query los ignora
   con un flag explícito `?include_untagged=true` si hace falta.

---

## 9. Cambios requeridos en `gt-mcp`

Resumen del scope técnico (bead correspondiente: ver §10):

1. `CreateIssue` en
   [service.rs:310](../crates/bins/gt-mcp/src/service.rs#L310) gana
   `domain: Vec<Domain>`, `surface: Vec<String>`,
   `depends_on: Vec<String>`, `role_scope: Option<Role>`.
2. Enum `Domain` derivado de §3 (closed set, `#[non_exhaustive]` no —
   queremos que agregar uno rompa compilación a propósito).
3. `validate()` chequea:
   - `domain[]` no vacío
   - `role_scope` compatible con `domain[]` (§4)
   - ciclos en `depends_on[]`
4. Schema de tabla `hq.issues` agrega columnas `domain_json`,
   `surface_json`, `depends_on_json` (JSON arrays — Dolt `JSON`).
5. Resources del §7 implementados como subgraph queries sobre Dolt.
6. `meta.report_gap` adopta el mismo schema (auto-mints heredan
   `domain: [meta.gap]`).

---

## 10. Bead que rastrea esta política

Esta directiva es **doc** — la implementación va en el epic
`hq-taxon-*` (a abrir cuando se priorice). Ver §9 para el scope.
