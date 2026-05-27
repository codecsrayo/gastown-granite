# 01 — Arquitectura

## Capas

```
┌──────────────────────────────────────────────────────────┐
│  bins/        composition roots (gt, gt-web, gt-replay…)   │
│               conocen TODOS los dominios; los cablean       │
├──────────────────────────────────────────────────────────┤
│  dominios/    vertical slices aislados                      │
│               gt-agent · gt-scheduling · gt-merge ·         │
│               gt-patrol · gt-orchestration · gt-quota ·     │
│               gt-feed                                       │
├──────────────────────────────────────────────────────────┤
│  kernel/      contratos + transportes + adaptadores BD      │
│               gt-events · gt-bus · gt-audit · gt-channel ·  │
│               gt-beads · gt-workspace · gt-telemetry ·      │
│               gt-plugin · gt-store-{dolt,pg}                │
└──────────────────────────────────────────────────────────┘
```

## Regla de dependencias (forzada por Cargo)

- Un **dominio** depende **solo del kernel**. Si `gt-merge` intenta
  `use gt_agent::…`, aparece como entrada ilegal en `Cargo.toml` y se ve en review.
- Los dominios se comunican **exclusivamente por eventos** en el bus, nunca por
  llamada directa.
- Los **adaptadores de BD** (`gt-store-*`) dependen de los dominios e implementan
  sus traits de repositorio. La dependencia se invierte: el dominio define el
  *puerto*, la infraestructura la *implementación*.
- `gt-feed` es un caso especial: depende **solo de `gt-audit`** (lee el log
  type-erased, no conoce ningún dominio). Ver [06-observability.md](06-observability.md).

## Ports & Adapters (hexagonal)

Cada dominio define su puerto como trait:

```rust
// gt-agent/src/repo.rs
pub trait SessionQueries {           // usado por GENÉRICOS, no dyn
    async fn active_sessions(&self) -> Result<Vec<Session>, AppError>;
}
```

El adaptador lo implementa en el kernel:

```rust
// gt-store-dolt/src/beads_repo.rs
impl SessionQueries for DoltRepo { /* … */ }
```

Beneficio: cada dominio se testea con un repo in-memory sin levantar Dolt/Postgres,
igual que los tests de DTO no necesitan base de datos.

## Modelo de actores (estado mutable)

No se comparte estado con `Arc<Mutex<T>>`. Cada dominio con estado mutable tiene **una
task que lo posee** (`actor.rs`); el resto le envía mensajes por `mpsc`.

```rust
// el SessionRegistry vive en UNA task; nadie más lo toca
enum AgentMsg {
    Add(Session),
    Remove(String),
    Snapshot(oneshot::Sender<Vec<Session>>),
}
```

Esto elimina:
- el problema de sostener un `Mutex` guard a través de `.await` (no es `Send`),
- la propagación de bounds `Send + Sync`,
- la mayoría de las quejas del borrow checker (el flujo de datos se vuelve explícito:
  los datos *se mueven* por canales, no *se comparten* por referencia).

Para datos read-heavy casi inmutables (config, plan de rotación), usar `arc-swap`:
lectores sin lock, el escritor reemplaza el `Arc` entero.

## Async en los bordes, síncrono en el núcleo

| async (I/O / espera) | síncrono (puro) |
|---|---|
| `gt-store-*`, `gt-web`, `gt-channel` | todos los `model.rs` |
| `supervisor`/`probe` (procesos, red) | cálculo de planes, derivación de estado |
| el relay del bus a tasks de I/O | serde, máquinas de estado, matching, `Command::{validate,execute}` |

Razón: una `async fn` "colorea" a todos sus callers (deben `.await`). Mantener el
núcleo síncrono evita ese contagio y permite **replay determinista** del log de
eventos (ver [06-observability.md](06-observability.md)).

Corolario (regla de determinismo): el núcleo síncrono tampoco lee **reloj** ni **random**.
El tiempo y los ids entran por el `Envelope`, generados en el borde; los timeouts son
**eventos** emitidos por productores async, no cálculos del núcleo. Por eso el `trait Command`
([09-llm-integration.md](09-llm-integration.md)) es sync y sin `#[async_trait]` — `dyn` +
`#[async_trait]` siguen confinados a `gt-plugin`. Detalle en [06-observability.md](06-observability.md).

Un único `tokio::Runtime`, creado en los binarios (`bins/`). Los crates de dominio
**no** crean runtime: reciben handles. Así un dominio se testea con `#[tokio::test]`
aislado.

## Dónde se permite `dyn` + `#[async_trait]`

Solo en `gt-plugin` (watchdogs, sheriffs — conjunto heterogéneo en runtime). En el
núcleo se usa **dispatch estático (genéricos)**, donde `async fn` en trait es nativo
(estable desde Rust 1.75) y no hay boxing.
