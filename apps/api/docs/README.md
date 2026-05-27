# gastown-rs — Documentación de implementación

Migración del orquestador de agentes **Gas Town** (Go) a una API/runtime en **Rust**,
con arquitectura event-driven, dominios aislados y persistencia híbrida.

Esta carpeta describe **cómo debería quedar la implementación**: las fronteras, los
contratos y las decisiones de diseño que el código debe respetar. No es un tutorial;
es la especificación de referencia.

## Índice

| Doc | Contenido |
|---|---|
| [01-architecture.md](01-architecture.md) | Capas (kernel / dominios), regla de dependencias, ports & adapters, modelo de actores, async-en-los-bordes |
| [02-tree.md](02-tree.md) | Árbol completo del workspace |
| [03-events.md](03-events.md) | Modelo de eventos: enums owned, envelope con causación, bus síncrono, dead-letter |
| [04-persistence.md](04-persistence.md) | Persistencia híbrida Dolt / Postgres (audit en `JSONB`); puertos y adaptadores; outbox; trazabilidad Grafana vía OTEL/Tempo |
| [05-queues.md](05-queues.md) | Taxonomía de colas, cola de trabajo sobre Dolt, claim por CAS, Dolt vs InnoDB, backpressure |
| [06-observability.md](06-observability.md) | Seguimiento de errores semánticos: causación, state machines, expectations, replay determinista |
| [07-frontend.md](07-frontend.md) | `gt-web`: API + SSE, snapshot vs stream |
| [08-getting-started.md](08-getting-started.md) | Hoja de ruta de implementación: pasos, entregables y gates de validación |
| [09-llm-integration.md](09-llm-integration.md) | Integración con modelos: `gt-mcp`, patrón `Command { validate, execute }`, scopes |

## Alcance y objetivo

La ventaja de esta migración **no es rendimiento**. gastown es un orquestador
I/O-bound (spawnea procesos, espera APIs de LLM, vigila archivos); el cuello de
botella nunca es la CPU del orquestador, y Go encaja ese workload de forma natural.

El valor real es:

1. **Eliminar clases de bugs** (nil-pointer / SIGSEGV de Dolt, fragilidad de
   subprocesos `bd`) vía `Option`/`Result` y un cliente nativo.
2. **Fronteras forzadas por el compilador** (aislamiento de dominios, enums
   exhaustivos de eventos) que en Go solo existirían por disciplina.
3. **Maestría en Rust** sobre un sistema real y complejo.

Decisión consciente a sostener: gastown es un proyecto vivo; reescribir persigue
un blanco móvil. El 80 % de la ganancia *arquitectónica* (aislamiento, ports/adapters,
CQRS) es independiente del lenguaje.

## Principios no negociables

- **Los dominios dependen solo del kernel.** Nunca un dominio importa otro.
- **Las BD nunca se exponen.** Los dominios definen traits de repositorio; los
  adaptadores `gt-store-*` los implementan (dependencia invertida).
- **Async en los bordes, síncrono en el núcleo.** La lógica pura no es async.
- **Estado mutable dentro de un actor**, coordinado por canales — no `Arc<Mutex>`
  repartido.
- **Eventos como enums owned**, no trait objects con lifetime.
- **Dolt es la fuente de verdad de los beads** y de la federación Wasteland.