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

## 2. Patrón validate → execute y convención `.` ↔ `_` (`hq-mcp-onboard.2`)

Todo tool mutador en `gt-mcp` viene como **par**: `<tool>.validate` y
`<tool>.execute`. La convención no es decorativa — refleja cómo los actores
reciben el comando: `validate` chequea precondiciones sin tocar estado;
`execute` corre la misma validación adentro del actor y luego aplica el
efecto. Detalle del *por qué* (audit, replay, invariantes) en
[§3](#3-why-rationale-hq-mcp-onboard3).

### 2.1 Cuándo correr `validate`

Por defecto **no hace falta**: `execute` valida internamente y devuelve el
mismo error si la precondición no se cumple. Úsalo solo en:

- **Preview** — quieres mostrar al usuario "esto haría X, ¿confirmas?" sin
  consumir slot ni emitir evento.
- **Pipelines** — chequeo barato antes de gastar capacidad (e.g. `enqueue`
  contra slot ya lleno).
- **Form validation** — UI que quiere feedback inmediato sobre `id` libre,
  `priority` en rango, `rig` registrado, etc.

`validate` **nunca** muta. Si el actor devuelve `ok` y luego corres `execute`,
puede fallar igual: hay una carrera entre ambos llamados. La invariante real
sigue siendo la chequeada *dentro* de `execute`.

### 2.2 Cuándo correr `execute` directo

Caso normal. El actor corre validate adentro de la misma transacción que el
efecto, así que **no hay** ventana entre check y apply. Si el chequeo falla,
te llega el mismo error que daría `validate` — sin estado mutado, sin evento
emitido, sin slot consumido.

### 2.3 Conversión `.` ↔ `_`

El nombre **canónico** (en docs, eventos, código Rust, schemas de actor)
usa puntos:

```
scheduling.create_bead.execute
patrol.register.execute
agent.transition.validate
```

El runtime de Claude Code expone los mismos tools con guiones bajos y prefijo
de server:

```
mcp__gt-mcp__scheduling_create_bead_execute
mcp__gt-mcp__patrol_register_execute
mcp__gt-mcp__agent_transition_validate
```

Reglas exactas:

| Forma canónica | Forma runtime |
|---|---|
| `<dominio>.<accion>.<fase>` | `mcp__<server>__<dominio>_<accion>_<fase>` |
| `.` (separador) | `_` (en runtime) |
| `<server>` ausente (implícito) | `mcp__<server>__` prefijo explícito |
| `accion` con guión bajo (`create_bead`) | igual (`create_bead` no se altera) |

El `accion` puede contener `_` propios (`create_bead`, `mark_dispatched`,
`set_prefix`, `complete_member`) — esos guiones bajos **son parte del nombre
de la acción** y no separadores. Reconstruir el nombre canónico desde el
runtime requiere conocer dónde corta `dominio`/`accion`/`fase`. Heurística:
`fase` siempre es la última pieza y es `validate` o `execute`; `dominio`
es la primera pieza; lo del medio es `accion` completa (incluyendo sus
`_` internos).

Ejemplo:

```
mcp__gt-mcp__scheduling_create_bead_execute
└── server ─┘└─ dom ─┘└────── accion ──────┘└ fase ┘

→ scheduling.create_bead.execute
```

### 2.4 Cómo lo ves en logs

Los eventos del event-log usan **siempre** la forma canónica con puntos:

```
2026-05-29T23:38:01Z scheduling.dispatched bead=hq-mcp-issues.1 worker=claude-host
2026-05-29T23:48:35Z agent.transition.executed id=hq-mcp-onboard from=open to=working
```

Si necesitas correlacionar un tool call de Claude Code con su evento, mapea
el `mcp__<server>__a_b_c_phase` → `a.b_c.phase` con la regla de §2.3.

---

## 3. Why-rationale: por qué todo pasa por MCP (`hq-mcp-onboard.3`)

Hay APIs alternas para casi todo lo que MCP expone: `docker exec dolt sql`,
edición directa de jsonl, scripts contra el event-log, llamadas HTTP a
`gt-api`. Funcionan. Y casi todas están **prohibidas** para agentes. Cuatro
razones, en orden de importancia:

### 3.1 Audit

Cada `execute` deja un evento en el event-log con `actor`, `target`, payload,
y timestamp. El log es la **única** fuente de verdad sobre qué pasó y quién
lo hizo. Si un cambio entra por fuera (Dolt SQL, jsonl manual), no hay
evento — y para el resto del sistema **no pasó**. Forensics, replay, y
reconciliación quedan inservibles.

Concretamente: cuando una corrida se cae a la mitad o un agente actúa
incorrectamente, lo primero que se pregunta es "muéstrame el orden exacto
de comandos". Si tu cambio no está en ese orden, sales del modelo causal y
nadie puede ayudarte (incluido tú mismo, una hora después).

### 3.2 Scope (RBAC)

Cada tool tiene un scope asociado en `mcp-scope.toml`. El server chequea el
scope contra el rol del caller antes de despachar al actor — un sheriff no
puede correr `quota.rotate`, un agent normal no puede `merge.fail`, etc.
Bypassear MCP es bypassear el RBAC. La consola de operador (`docker exec`)
existe porque alguien tiene que poder romper el sistema; no porque sea el
camino normal.

Regla: si tu identidad pide un tool y el server dice `403`, el problema es
de scope, no del tool. Abre bead pidiendo el scope, no fuerces el camino.

### 3.3 Invariantes

Los actores son los únicos owners legítimos de su estado (scheduling owns la
queue, patrol owns leases, merge owns slots). Toda mutación pasa por una
mailbox **serializada** — el actor procesa un comando a la vez. Eso te da
invariantes de máquina-de-estados gratis: no puedes hacer `transition X→Y`
mientras alguien más hace `transition X→Z` con el mismo recurso; uno gana,
el otro ve el estado nuevo y falla limpio.

Si saltas el actor (Dolt SQL directo), pisoteas la mailbox: dos escrituras
concurrentes pueden corromper el estado sin que nadie emita evento de error.
Y si el actor mantiene caché en memoria (sessions, slots, leases), tu
escritura ni siquiera será visible hasta el próximo rebuild — ver memoria
`Dolt split-brain 2026-05`.

### 3.4 Replay

El event-log es replay-able: dado un snapshot vacío y la secuencia de
eventos, reconstruyes el estado actual completo. Eso permite migraciones,
debugging post-mortem, tests de regresión basados en logs de producción, y
recovery limpio tras corrupción de Dolt.

Cualquier cambio que no pasa por un tool MCP **rompe el replay** — porque
no está en el log y el estado reconstruido divergerá del actual. Cuando
eso pasa en producción no se nota inmediatamente; se nota meses después
cuando algo no cuadra y nadie recuerda qué se editó a mano.

### 3.5 Resumen operativo

| Acción | Quién | Vía |
|---|---|---|
| Cambiar estado observable | Agente | MCP `<tool>.execute` |
| Leer estado canónico | Agente | MCP recurso (`gt://*`) |
| Romper invariante (emergencia) | Operador | `docker exec dolt sql` |
| Reconstruir estado | Sistema | replay del event-log |
| Inspeccionar histórico | Operador | dolt diff / event-log query |

Si te encuentras escribiendo SQL para Dolt o `bd` directamente, **detente**:
o estás en escape hatch de operador (autorización explícita del humano), o
estás creando un hueco que será imposible de debuggear después. La salida
correcta es bead — ver §4.

---

## 4. Gap discipline: qué hacer cuando el tool no existe (`hq-mcp-onboard.4`)

Tarde o temprano vas a necesitar una operación y `tools/list` no la va a
tener. Es esperado — MCP cubre la superficie estable, no toda combinación
posible. La regla es: **el tool faltante se documenta como hueco, no se
sortea**.

### 4.1 Síntomas de hueco

Cualquiera de estos es señal:

- Buscas en `gt-mcp-cli tools` y el dominio no existe (`issues.*`,
  `feed.*`, `mayor.*`).
- El dominio existe pero no la fase que necesitas (solo `create`, no
  `update`; solo `register`, no `retire`).
- El tool acepta un payload, pero falta el campo que necesitas para el
  caso (`scheduling.create_bead` no acepta `assignee` — solo `id`,
  `title`, `priority`).
- El recurso (`gt://*`) trae un snapshot pero no las filas filtradas que
  necesitas (no hay `?status=working` server-side).

### 4.2 Anti-patrones (no hagas esto)

| Atajo | Por qué no |
|---|---|
| `docker exec dolt sql "UPDATE ..."` | Escape hatch de operador, no de agente. Rompe audit + replay (§3.1, §3.4). |
| Editar `*.jsonl` o `bd export` a mano | Mismo problema + race con `bd auto-export` (memoria `bd auto-export throttle race`). |
| Llamar `gt-api` HTTP directo | Bypassea RBAC y emite eventos con `actor` mal atribuido. |
| Inventar un script wrapper que hace el mutación "rápida" | Crea camino paralelo que nadie sabe que existe; siguiente agente lo encuentra y lo copia. |
| Saltar al rol de operador para "solo esta vez" | No tienes el rol; pedir al humano que corra el comando es válido, hacerlo tú no. |

### 4.3 Camino correcto: abrir bead

1. **Identifica el hueco preciso.** Nombre canónico que tendría el tool:
   `<dominio>.<accion>.<fase>` (ver §2.3). Si el dominio no existe, propón
   el nombre. Si solo falta una fase o campo, sé específico.

2. **Diseña el payload mínimo.** Qué inputs necesita, qué invariantes
   chequea, qué evento emite, qué scope corresponde. No tiene que estar
   perfecto — tiene que ser suficiente para que quien implemente entienda
   la forma.

3. **Crea el bead** vía `scheduling.create_bead.execute` (id, title,
   priority). Usa prefijo del rig (`hq-` para HQ). Ejemplo:

   ```
   id:       hq-mcp-issues.2
   title:    issues.create.{validate,execute} MCP tool
   priority: 1
   ```

   > Mientras `issues.*` no exista vía MCP, los campos extra (description,
   > acceptance criteria, design) se quedan vacíos hasta que `issues.update`
   > esté disponible (`hq-mcp-issues.3`). Es OK — el bead existe en `pending`
   > y queda en el catálogo.

4. **Bloquea el trabajo dependiente.** Si tu tarea actual depende del hueco,
   regístralo en el bead-padre (`notes`) o como bead intermedio con
   `external_ref` al hueco. **No** sigas como si el camino existiera — si
   intentas continuar con un workaround, ese workaround se queda.

5. **Reporta** (opcional, automatizable). El tool `report_gap` (`hq-mcp-
   onboard.8`) cierra este loop: el agente lo invoca con el dominio/acción
   faltante y el server abre el bead automáticamente con la sesión actual
   como `created_by`. Hasta que aterrice, paso 3 manual.

### 4.4 Cuándo el "hueco" no es hueco

Antes de abrir bead, verifica que no se trata de:

- **Tool deferred no cargado** — está en la lista pero el schema no.
  Cárgalo con `ToolSearch query="select:<nombre>"` (ver §1.2).
- **Tool con otro nombre** — la convención `dominio.accion` puede no
  coincidir con tu intuición. Revisa `gt-mcp-cli tools` completo.
- **Recurso en vez de tool** — algunas lecturas son recursos, no tools.
  Revisa `gt-mcp-cli resources`.
- **Operación derivable** — a veces el efecto que buscas se logra
  combinando dos tools existentes. Si dudas, pregunta antes de abrir
  bead.

Si después de revisar sigue siendo un hueco real, paso 3.

### 4.5 Quién resuelve el bead

El bead entra a la cola normal: dispatcher claim, agente trabaja, merge,
close. No hay vía rápida — los huecos de MCP los implementa quien tenga
contexto del actor afectado (el dominio del tool faltante), igual que
cualquier otra feature.

Mientras tanto, si el bloqueo es operacional y urgente, **escala al
humano**: él puede correr el escape hatch (operator-only) sin romper la
disciplina del sistema. Tú no.

---

## 5. In-session vs out-of-session (`hq-mcp-onboard.5`)

"Agente" no es una sola cosa. Hay al menos dos modos de operar y los tools
disponibles, el flujo de errores, y la forma de reportar progreso cambian
entre ambos. Saber en cuál estás ahorra confusiones.

### 5.1 In-session (Claude Code)

Sesión interactiva o headless de Claude Code (lo que está leyendo este
documento ahora mismo). Tools MCP llegan **inyectados al runtime** — los ves
como `mcp__gt-mcp__<...>` en el catálogo de tools. El runtime gestiona la
conexión HTTP, los timeouts, y los reintentos.

Características:

- Tools MCP están deferred al inicio; se cargan vía `ToolSearch` por nombre
  o keyword (§1.2).
- Recursos se leen con `ReadMcpResourceTool` / `ListMcpResourcesTool` —
  primitivas del runtime, no tools del server.
- Identidad del caller = sesión Claude (no agente Gas Town). El `actor` que
  llega al server depende de cómo está configurada la conexión (stdio
  hereda env; HTTP usa headers).
- Errores llegan como tool_use_error con el JSON del server adentro;
  reintenta si es transitorio, abre bead si es scope o gap.
- No tienes acceso a HTTP raw — el runtime tiene su capa MCP propia.

**Ejemplo**: claim de bead desde el host (esta sesión) → tools
`mcp__gt-mcp__*` directos. Si el tool no existe, opciones son `ToolSearch`,
abrir bead, o escalar al humano (no `curl`).

### 5.2 Out-of-session (CLI, scripts, agentes no-Claude)

Cualquier cosa que **no** sea Claude Code: shell del operador,
job programado, agente custom, gateway. Acceden al server vía:

- **`gt-mcp-cli`** — Rust client en `/home/nixos/gt-mcp-cli`, instalado
  vía cargo. Subcomandos: `tools`, `resources`, `call`, `read`.
- **HTTP raw** — POST a `http://127.0.0.1:8765/mcp` con frames JSON-RPC
  streamable. Útil cuando necesitas control fino o desde un lenguaje que
  no tiene cliente MCP.
- **Otro cliente MCP** — Cursor, Inspector, etc. Mismas reglas.

Características:

- Catálogo se descubre por llamada explícita (`tools/list`, `resources/list`),
  no inyección runtime.
- Identidad la pones tú vía headers de la conexión (`X-GT-Actor`,
  `X-GT-Role`); sin eso, el server asume default (operator/anon, depende
  config).
- Errores son JSON-RPC; sin runtime que los empaquete, los manejas tú.
- Conexión es tu responsabilidad: reconectar tras restart, manejar timeouts,
  no pisar el slot del dispatcher si encolas en bucle.

**Ejemplo**: smoke test de un nuevo tool. Operador corre:

```bash
gt-mcp-cli call scheduling.create_bead.execute \
  --arg id=test-smoke-1 \
  --arg title="smoke test" \
  --arg priority=2
```

Out-of-session también es el modo correcto para **automation** que corre
fuera de Claude (cron, hooks, sheriffs). Si necesitas que algo pase sin un
agente humano, va por CLI o HTTP, no por una sesión Claude headless.

### 5.3 Reglas que cambian entre modos

| Aspecto | In-session | Out-of-session |
|---|---|---|
| Discovery | `ToolSearch` + runtime inject | `tools/list` HTTP |
| Identidad | runtime + env de sesión | headers explícitos |
| Errores | tool_use_error | JSON-RPC raw |
| Reintentos | runtime parcial | tú mismo |
| Recursos | `Read/ListMcpResourceTool` | `resources/read`, `resources/list` |
| Disponibilidad | requiere sesión viva | persistente |

### 5.4 Cuál usar para qué

- **Trabajo de agente** (claim, transition, commit): in-session.
- **Bootstrap, smoke tests, reproducir bugs**: out-of-session (CLI).
- **Integración con CI / cron / hooks externos**: out-of-session (CLI o
  HTTP).
- **Operador haciendo recuperación manual**: out-of-session + escape hatch
  Dolt si MCP no llega.

Si te pillas mezclando (sesión Claude que abre un shell que llama
`gt-mcp-cli` para hacer lo que ya podría hacer in-session) — para. Es
señal de que falta un tool, o de que estás duplicando RBAC, o de que algo
del runtime no se cargó. Diagnostica antes de codificar el atajo.

---

## 6. Memory frontmatter: version/status field (`hq-mcp-onboard.6`)

Las memorias de auto-memory (`~/.claude/projects/<proj>/memory/*.md`) son
fundamentales pero **se vuelven obsoletas en silencio**. Un agente futuro
las lee como verdad, y si la realidad cambió (tool shipped, archivo
movido, decisión revisada), actúa sobre información muerta.

Esta sección define el contrato para mantener memorias sanas en el tiempo.

### 6.1 Frontmatter extendido

Hasta hoy:

```yaml
---
name: my-memory
description: ...
metadata:
  type: feedback | project | user | reference
---
```

A partir de este bead, agregamos dos campos opcionales en `metadata`:

```yaml
---
name: my-memory
description: ...
metadata:
  type: feedback | project | user | reference
  status: current | historical | superseded
  superseded_by: name-of-replacement-memory   # solo si status=superseded
---
```

### 6.2 Estados

| Estado | Significado | Cuándo aplica |
|---|---|---|
| `current` (default si ausente) | Cierto al día de hoy, accionable. | Memoria nueva, o vieja pero todavía verificable. |
| `historical` | Cierto en su momento, sigue útil como contexto, **no** accionable. | Decisiones pasadas, snapshots de estado superados pero referenciados. |
| `superseded` | Reemplazada por otra memoria. **No leer**, ir a `superseded_by`. | Refactor de memoria, fix de error, división en submemorias. |

Reglas:

- **`current`** es default. Si la frontmatter no tiene `status`, asume
  `current`. Migración no requerida — la ausencia es válida.
- **`historical`** se usa para preservar contexto sin guiar acción. Ejemplo:
  "X se decidió en marzo, después se revirtió en abril" — la decisión de
  marzo es historical, la de abril es current.
- **`superseded`** apunta a `superseded_by` (slug de otra memoria). El
  reader debe seguir el enlace y leer la nueva. Mantener la superseded
  evita romper backlinks `[[old-name]]` y conserva trazabilidad.

### 6.3 Transiciones

Cuándo cambiar el status:

- **`current` → `historical`** cuando la condición que motivó la memoria
  cambia pero el contexto sigue importando. Ejemplo: memoria
  `bd-export-after-create` se marcó `historical` cuando MCP reemplazó a
  `bd via docker exec` — sigue útil para entender el flujo Go-era.
- **`current` → `superseded`** cuando una memoria nueva reemplaza
  completamente la vieja. Crea la nueva, marca la vieja como `superseded`,
  llena `superseded_by`. **No la borres** — preserva backlinks.
- **`historical` → `superseded`** raro; pasa cuando una memoria histórica
  se reescribe como contexto consolidado.
- **`superseded` → cualquier otro** nunca; superseded es terminal.

### 6.4 Quién marca y cuándo

- Cuando ediTAS una memoria existente porque la realidad cambió, evalúa si
  es un **update in-place** (mantener `current`, cambiar el body) o un
  **supersede** (memoria nueva + marcar vieja `superseded`).
  - In-place: corrección menor, ajuste de fecha, fix de typo.
  - Supersede: cambio de premisa, división en varias memorias, fusión con
    otra.
- Cuando observas que una memoria `current` ya no aplica (por ejemplo:
  describe un archivo que ya no existe, o un tool que cambió de nombre):
  - Si la información histórica sigue útil: márcala `historical` y agrega
    una nota corta al body indicando qué la reemplaza.
  - Si no aporta nada: bórrala (la regla `What NOT to save` aplica
    retroactivamente).

### 6.5 Cómo lo usa el reader

Cuando cargas memorias al inicio de una sesión (índice en `MEMORY.md`):

1. Si la entry está en `MEMORY.md`, leela.
2. Al abrirla, mira `metadata.status`:
   - `current` o ausente: trátala como fuente de verdad operativa.
   - `historical`: úsala como contexto, **no** bases acción inmediata sin
     verificar contra el estado actual del repo / Dolt / MCP.
   - `superseded`: salta a `superseded_by` y léela. Si la nueva no existe,
     es un dangling pointer — registra como hueco.

Antes de actuar sobre una memoria que menciona un archivo, tool, o flag
específico, verifica que sigue existiendo (regla `Before recommending from
memory`). El status field es **anuncio**, no garantía: una memoria
`current` puede igual estar desactualizada si nadie la revisó.

### 6.6 Migración del índice

`MEMORY.md` no necesita migración. Las entries ahí son one-liners; el
status vive en la memoria misma. Si un entry apunta a una memoria
`superseded`, mantenlo (los backlinks de otras memorias todavía pueden
seguir el slug); cuando edites el índice por otra razón, puedes inlining
el cambio a la entry nueva.

---

## Secciones pendientes

- `hq-mcp-onboard.7` — tool `help` (índice + URIs + version inline)
- `hq-mcp-onboard.8` — tool `report_gap`
- `hq-mcp-onboard.9` — consolidación + índice + cross-links
- `hq-mcp-onboard.10` — CLAUDE.md global + proyecto apuntan acá
