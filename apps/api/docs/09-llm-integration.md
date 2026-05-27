# 09 — Integración con modelos (LLM como cliente remoto)

## Premisa

Un modelo (agente LLM) opera **fuera del contenedor**. No tiene shell ni acceso al
filesystem; alcanza el sistema solo por un protocolo de red. Necesita poder:

- Leer el estado actual.
- **Validar** una acción antes de ejecutarla, sin efectos colaterales.
- Ejecutar acciones y observar sus consecuencias en vivo.
- Auditar qué hizo (él u otros agentes).

La arquitectura ya soporta esto casi por completo. Este documento describe la única
adición no trivial — el patrón `Command { validate, execute }` — y el binario `gt-mcp`
que expone los dominios como herramientas para el modelo.

## Lo que ya está alineado

| Pieza existente | Cómo sirve al modelo |
|---|---|
| **Commands como enums tipados** | Se mapean 1:1 a tool schemas (JSON Schema generado desde el enum). |
| **Composition root pattern** | Añadir `bins/gt-mcp` es solo otro cableador; no toca dominios. |
| **`gt-web` + SSE** | Lectura snapshot y stream en vivo ya disponibles vía HTTP. |
| **Audit + envelope con causación** | Todo lo que haga el modelo queda trazado por construcción. |
| **State machines + dead-letter** | Acciones ilegales se rechazan; commands sin handler se ven. |
| **`gt-replay`** | El modelo puede pedir reproducción determinista de un log. |

## La pieza que falta: `Command { validate, execute }`

Hoy los commands ejecutan. Para que un modelo *valide sin efectos*, cada command separa
explícitamente las dos fases:

```rust
// gt-events/src/command.rs
#[async_trait]
pub trait Command {
    type Output;
    type State;

    /// Comprueba si el command sería aceptado dado el estado actual.
    /// SIN EFECTOS COLATERALES. Puede inspeccionar `state` pero no mutarlo.
    async fn validate(&self, state: &Self::State) -> Result<(), AppError>;

    /// Ejecuta el command. Solo llamar tras `validate` exitoso
    /// (o asumiendo la validación implícita).
    async fn execute(&self, state: &mut Self::State) -> Result<Self::Output, AppError>;
}
```

Cada `*Msg` de los dominios implementa `Command`. El actor procesa el mensaje llamando
`validate` y luego `execute`; el modelo puede llamar **solo** `validate` para "preguntar
sin hacer".

Retrofittear esto después es invasivo: hay que escribirlo desde el Paso 2 de la hoja de
ruta para todos los commands nuevos.

## Binario `gt-mcp`

Sibling de `gt`, `gt-web` y `gt-replay`. Importa los dominios igual que `gt-web`, pero
expone los commands vía **MCP (Model Context Protocol)** — el estándar JSON-RPC para
exponer herramientas a modelos.

```
bins/gt-mcp/
└── src/
    ├── main.rs        # arranca el server MCP (stdio o HTTP+SSE)
    ├── tools.rs       # registro de herramientas (un Command → un tool MCP)
    ├── schema.rs      # JSON Schema generado desde los enums de commands
    ├── auth.rs        # scopes por identidad de agente
    └── audit.rs       # cada invocación → evento al audit log
```

El registro de tools se deriva del tipo, no se escribe a mano:

```rust
// patrón aproximado
register_tool::<AgentMsg::Spawn>("agent.spawn");      // execute + validate
register_tool::<AgentMsg::Kill>("agent.kill");
register_tool::<SchedMsg::Enqueue>("scheduling.enqueue");
// …
register_query::<SessionQueries>("agent.sessions");   // solo lectura
register_replay("replay");                            // gt-replay como tool
```

Cada tool tiene automáticamente dos variantes: `*.execute` y `*.validate`.

## Capas de autorización (en la frontera)

Mismo principio que en `gt-web`: la frontera decide; los dominios no saben de auth.
`gt-mcp` resuelve la identidad del agente al inicio de la conexión y le adjunta un
**scope**:

```rust
pub struct Scope {
    pub actor: String,                  // id del agente
    pub allow: BTreeSet<String>,        // tools permitidos (glob: "agent.*", "scheduling.enqueue")
    pub validate_only: bool,            // si true, solo *.validate
}
```

Un command fuera del scope no se envía al actor: se rechaza en `gt-mcp` y se emite
`UnauthorizedCommand` al audit. Esto permite políticas tipo:

- *"Este modelo solo puede leer y validar"* → `validate_only = true`.
- *"Ese modelo puede dispatch pero no rotation"* → `allow = {"scheduling.*", "agent.*"}`.
- *"Modelo de auditoría: solo queries y replay"* → `allow = {"*.sessions", "replay"}`.

## Casos de uso cubiertos

| Petición del modelo | Cómo cae en la arquitectura |
|---|---|
| "Dame el estado actual" | Query snapshot por MCP / `gt-web` |
| "¿Esta acción sería aceptada?" | `Command::validate` sin efectos |
| "Ejecuta esta acción" | `Command::execute` + scope check + audit |
| "Observa los efectos en vivo" | Suscripción SSE / MCP stream a `EventRecord` |
| "Reproduce este bug con este log" | `gt-replay` expuesto como tool MCP |
| "¿Qué hizo el agente X en la última hora?" | Query del audit por `actor` + `correlation_id` |
| "Simula esta rotación de cuenta" | `gt-quota::orchestrator` con `--only` + `validate` |

## Combinación poderosa: simulación sobre el log

Replay determinista + `validate` habilita un caso que no es obvio:

```
1. Modelo pide log histórico desde T.
2. Modelo describe una acción hipotética en T+Δ.
3. gt-mcp re-corre el log por la lógica pura hasta T+Δ-1.
4. Llama validate(action) contra el estado reconstruido.
5. Devuelve "sería aceptada / rechazada por X razón".
```

El modelo razona sobre estados que **nunca ocurrieron en producción**, sin tocar
producción. Es lo que hace al sistema seguro de delegar a un agente.

## Adiciones al árbol

```
crates/kernel/
└── gt-events/src/
    └── command.rs               # trait Command { validate, execute }

bins/
└── gt-mcp/                      # binario MCP (sibling de gt-web)
    └── src/{main, tools, schema, auth, audit}.rs
```

## Lo que se evita por construcción

- **El modelo nunca entra al contenedor.** Solo habla MCP/HTTP.
- **Comandos ilegales no llegan al dominio.** Auth en la frontera + state machines
  rechazan en el actor.
- **Acciones silenciosas.** Cada invocación es un evento auditado con `actor` y
  `correlation_id`.
- **Acoplamiento del modelo a tipos internos.** Solo ve los DTOs/tools expuestos, igual
  que un cliente HTTP.

La arquitectura no se redefine para soportar agentes LLM — se *despliega* a ese caso de
uso porque las decisiones que tomaste por otras razones (commands tipados, actors,
audit, replay, frontera única) son justo las que un modelo necesita para operar de forma
segura y verificable.
