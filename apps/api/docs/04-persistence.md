# 04 — Persistencia

## Híbrido de dos motores

El criterio: **casi todo es un bead, y los beads viven en Dolt.** Postgres cubre lo que
*no* es un bead. No hay tercer motor: el log de eventos vive en Postgres (JSONB), no en
Mongo (ver más abajo).

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
  gt-audit          (EventRecord en columna JSONB, payload sin tipar, append-heavy)
  gt-feed           (proyecciones read-side; vistas / vistas materializadas sobre el log)
```

> Por qué se conserva Dolt: el versionado por-fila, el `rollback` y la federación
> Wasteland (push/pull contra DoltHub) **son features**, no detalles. Postgres no las
> replica sin reconstruirlas (temporal tables / replicación propia).

### Por qué Postgres-JSONB para el audit y no Mongo

El payload del `EventRecord` es schema-variable (`type: String`, `payload: Map`), lo que
históricamente justificaba Mongo. Pero **Postgres `JSONB` cubre ese caso** (indexable por
GIN, consultable con operadores `->`/`@>`) sin sumar un tercer motor. Lo que inclina la
balanza es el **consumidor de trazabilidad: Grafana**.

- El datasource **MongoDB no está en el core OSS de Grafana** (es Enterprise de pago o un
  plugin community). El de **Postgres es de primera clase**: paneles SQL, time-series,
  variables — directo sobre la tabla de eventos.
- La trazabilidad *de cadenas causales* no se hace consultando documentos: se hace con
  **traces**. `gt-telemetry` ya exporta OTEL → la cadena (`correlation_id`/`causation_id`)
  aparece como árbol de spans en **Tempo**, y las métricas en **Prometheus**. Grafana lee
  los tres (Tempo + Prometheus + Postgres) de forma nativa. Mongo no aporta nada que estos
  no cubran mejor (ver [06-observability.md](06-observability.md)).

Resultado: **un motor menos, un adaptador menos** (`gt-store-mongo` desaparece; su
`EventStore` lo implementa `gt-store-pg`) y *mejor* trazabilidad en Grafana, no peor.

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
| `gt-store-pg` | `AccountRepository` · `EventStore` | sqlx (runtime `query`/`query_as`, sin macros chequeados en compilación), `outbox`, tabla de eventos `JSONB` (audit) + proyecciones de feed |

> **Sin ORM (decisión).** El puerto del dominio es un trait con structs planos; meter un
> ORM (p. ej. SeaORM) obligaría a mantener entidades-ORM **en el adaptador** + mapeo a
> los structs del dominio (las entidades no pueden vivir en el dominio sin romper la regla
> de dependencias). Mismo mapeo que ya se hace a mano, más una dep pesada y un acople de
> build hacia herramientas extra. La carga real (INSERT append-only + rollups `SUM` + JSONB)
> rinde mejor con SQL crudo. Si el read-model de `gt-feed` crece relacionalmente complejo,
> se revisita.
>
> **Migraciones.** Schema versionado con `sqlx::migrate!("./migrations")`: el macro embebe
> los `.sql` leyendo solo el directorio en build-time (no requiere DB viva, igual que la
> elección de runtime queries). En runtime el `Migrator` registra cada versión aplicada en
> `_sqlx_migrations` y la valida por checksum — **nunca editar un archivo aplicado**, se
> agrega uno nuevo. `ensure_schema(pool)` ahora corre esa cadena, idempotente entre boots.

## Reglas de consistencia (2 motores)

1. **Dolt es la única fuente de verdad de los beads.** Nada más escribe beads.
2. **Cero transacciones cross-store.** No existe transacción que abarque Dolt + Postgres.
   Cada motor escribe lo suyo; se integran **por eventos en el bus**, nunca por escritura
   cruzada. (Quota y audit comparten Postgres, pero se tratan como tablas lógicamente
   separadas: el audit no participa en transacciones de dominio.)
3. **Outbox por cada store que escribe.** Se escribe la entidad + fila `outbox` en una
   transacción, y un relay publica al bus → de ahí cae a la tabla `JSONB` de audit en
   Postgres. Dolt es MySQL-compatible, así que soporta el patrón outbox igual que Postgres.
4. **El feed es read-only del stream.** Proyecta sobre la tabla de eventos; nunca escribe
   hacia atrás.
5. **El bus + el audit log son la espina dorsal de integración** entre motores que no se
   hablan por SQL.

## Idempotencia

Toda cola/outbox durable puede reentregar tras un crash (entrega *at-least-once*;
"exactly-once" no existe). El consumidor deduplica por `event_id` del envelope. Sin esto,
un reintento de dispatch spawnea el agente dos veces — bug semántico clásico.
