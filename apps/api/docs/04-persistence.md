# 04 — Persistencia

## Híbrido de tres motores

El criterio: **casi todo es un bead, y los beads viven en Dolt.** Postgres y Mongo solo
cubren lo que *no* es un bead.

```
DOLT (sistema de registro + federación)   ← versionado por-fila, rollback, DoltHub
  gt-agent          (issues, sessions)
  gt-scheduling     (queue, dispatch)
  gt-merge          (merge slots, MRs)
  gt-patrol         (escalations)
  gt-orchestration  (delegation, rig, convoy)
  Wasteland sync    ← LA RAZÓN de conservar Dolt

POSTGRES (relacional nuevo, no-bead, transaccional)
  gt-quota          (account state, RotationPlanSnapshot, ActiveSwaps)

MONGO (documentos schema-variable, append-heavy)
  gt-audit          (EventRecord, payload sin tipar)
  gt-feed           (proyecciones, read-side)
```

> Por qué se conserva Dolt: el versionado por-fila, el `rollback` y la federación
> Wasteland (push/pull contra DoltHub) **son features**, no detalles. Postgres+Mongo
> no las replican sin reconstruirlas (temporal tables / replicación propia).

## Puertos en los dominios, adaptadores en el kernel

La BD **no es un dominio**: es infraestructura. Cada dominio define su trait de
repositorio; el adaptador lo implementa. Dependencia invertida.

```rust
// dominio: define el puerto
// gt-quota/src/repo.rs
pub trait AccountRepository {
    async fn limited_accounts(&self) -> Result<Vec<Account>, AppError>;
    async fn record_swap(&self, swap: &Swap) -> Result<(), AppError>;
}

// kernel: implementa el adaptador
// gt-store-pg/src/quota_repo.rs
impl AccountRepository for PgRepo { /* … */ }
```

Adaptadores:

| Crate | Implementa | Detalle |
|---|---|---|
| `gt-store-dolt` | `BeadRepository` (todos los dominios bead) | cliente MySQL-wire (:3307), `commit`/`diff`/`rollback`, `wasteland_sync` |
| `gt-store-pg` | `AccountRepository` | SeaORM/sqlx, `outbox` |
| `gt-store-mongo` | `EventStore` | driver mongodb, proyecciones de feed |

## Reglas de consistencia (3 motores)

1. **Dolt es la única fuente de verdad de los beads.** Nada más escribe beads.
2. **Cero transacciones cross-store.** No existe transacción que abarque
   Dolt + Postgres + Mongo. Cada motor escribe lo suyo; se integran **por eventos en el
   bus**, nunca por escritura cruzada.
3. **Outbox por cada store que escribe.** Se escribe la entidad + fila `outbox` en una
   transacción, y un relay publica al bus → de ahí cae a Mongo (audit). Dolt es
   MySQL-compatible, así que soporta el patrón outbox igual que Postgres.
4. **El feed (Mongo) es read-only del stream.** Nunca escribe hacia atrás.
5. **El bus + el audit log son la espina dorsal de integración** entre motores que no se
   hablan por SQL.

## Idempotencia

Toda cola/outbox durable puede reentregar tras un crash (entrega *at-least-once*;
"exactly-once" no existe). El consumidor deduplica por `event_id` del envelope. Sin esto,
un reintento de dispatch spawnea el agente dos veces — bug semántico clásico.
