# Feature — Trazabilidad de tokens por sesión + predicción de bloqueo de cuenta

**Dominio:** `gt-quota` (Postgres). Extiende el estado de cuenta existente
(`Account` + `AccountQuotaStatus`, eventos `QuotaEvent { AccountLimited, Rotated, Blocked, Probed }`).

## Objetivo

Llevar trazabilidad del **consumo de tokens por sesión**, promediarlo, y **predecir cuánto
le falta a una cuenta para bloquearse** (rate-limit / cuota agotada). El valor no es el
contador en sí: es **rotar la cuenta *antes* del bloqueo**, no después. Hoy la rotación es
**reactiva** (dispara con `AccountLimited`/`Blocked`); esta feature la vuelve **predictiva**.

## Por qué importa

- Un bloqueo duro detiene a los polecats que usan esa cuenta hasta que la rotación reacciona
  → ventana de parada observable.
- Con consumo + ventana de límite se calcula un **ETA-al-bloqueo**; cruzando un umbral se
  emite aviso temprano y la cadena de rotación actúa con holgura.

## Modelo de datos (Postgres)

```sql
-- Append-heavy. Una fila por muestra/turno atribuido a (cuenta, sesión).
CREATE TABLE token_usage (
    id              BIGSERIAL PRIMARY KEY,
    account_id      TEXT        NOT NULL,
    session_id      TEXT        NOT NULL,   -- atribución por sesión (ver Riesgos)
    model           TEXT        NOT NULL,   -- consumo no es comparable entre modelos
    ts              TIMESTAMPTZ NOT NULL,
    input_tokens    BIGINT      NOT NULL,
    output_tokens   BIGINT      NOT NULL,
    cache_read      BIGINT      NOT NULL DEFAULT 0,
    cache_creation  BIGINT      NOT NULL DEFAULT 0
);
CREATE INDEX ON token_usage (account_id, ts);
CREATE INDEX ON token_usage (session_id, ts);
```

El **límite** vive en `Account` (presupuesto de la ventana + cuándo resetea):

```
Account {
    id
    window_kind        // p.ej. rolling-5h | weekly  (depende del plan)
    window_limit       // presupuesto de tokens (o unidad de coste) por ventana
    window_started_at  // inicio de la ventana actual
    window_resets_at   // cuándo se libera
    status             // de AccountQuotaStatus (Healthy | Limited | Blocked | Cooldown)
}
```

> El **costo no se mide en "tokens crudos" sumados a ciegas**: input, output y cache pesan
> distinto y varían por modelo. Normalizar a una **unidad de coste** por modelo (ponderación
> configurable) antes de comparar contra `window_limit`.

## De dónde salen los tokens (fuente de verdad)

Dos fuentes, complementarias:

1. **Cabeceras de rate-limit de la respuesta de la API** (autoridad real del proveedor):
   `anthropic-ratelimit-*-remaining` / `*-reset`. Es lo que el proveedor de verdad cuenta;
   el `probe.rs` de `gt-quota` ya sondea cuentas — extender para capturar remaining/reset.
2. **Conteo local por respuesta** (`usage` de cada llamada): atribuible a la **sesión**, que
   las cabeceras por sí solas no dan. Es lo que permite el desglose por sesión.

Regla: **las cabeceras del proveedor mandan** sobre el conteo local cuando difieren (el local
sirve para atribución y para suavizar entre sondeos).

## Eventos nuevos (bus)

```rust
// gt-quota/src/events.rs — añadir a QuotaEvent
QuotaEvent::TokensSampled {
    account: String,
    session: String,
    model: String,
    input: u64, output: u64, cache_read: u64, cache_creation: u64,
}
QuotaEvent::UsageProbed {          // desde probe.rs: snapshot de las cabeceras del proveedor
    account: String,
    remaining: u64, resets_at: OffsetDateTime,
}
QuotaEvent::BlockPredicted {       // cruzó el umbral de ETA → dispara rotación predictiva
    account: String,
    eta_to_block: Duration,
    consumed: u64, limit: u64, rate_per_min: f64,
}
```

`TokensSampled` cae al `outbox` → bus → tabla `token_usage` + audit (mismo patrón de
persistencia que el resto, ver [../04-persistence.md](../04-persistence.md)). `BlockPredicted`
lo consume la cadena de rotación (`orchestrator.rs` / `handlers/rotation.rs`).

## Cálculo de la predicción

Sobre la ventana **actual** de la cuenta:

```
consumed(account)  = Σ coste_normalizado(uso)  en [window_started_at, now]
remaining          = window_limit - consumed
rate_per_min       = EWMA( consumed / minutos_transcurridos )   // media móvil exponencial
eta_to_block       = remaining / rate_per_min                   // minutos hasta agotar
```

- **EWMA** (media móvil exponencial), no media simple: el consumo llega a ráfagas (varias
  sesiones a la vez); EWMA reacciona a tendencia sin que un pico la dispare.
- **Tope por la ventana:** si `now + eta_to_block > window_resets_at`, la cuenta **resetea
  antes** de bloquearse → no hay riesgo en esta ventana. Predecir bloqueo solo si el ETA cae
  *dentro* de la ventana vigente.
- **Promedio por sesión** (lo que pidió el objetivo): `rate` también se desglosa por sesión
  → "qué sesión está quemando la cuenta" + proyección por sesión, no solo agregada.

El cómputo es **lógica pura** (sync, sin reloj dentro del núcleo): el `now` y los timestamps
entran como datos del evento/probe; así la predicción es **replay-able** (regla de
determinismo, [../06-observability.md](../06-observability.md)).

## Acción: rotación predictiva

```
on TokensSampled | on UsageProbed | on tick
  → recompute rate + eta_to_block por cuenta
  → si eta_to_block < UMBRAL (configurable, p.ej. 15 min) y dentro de la ventana:
        emit BlockPredicted
  → handlers/rotation.rs: rota a una cuenta sana ANTES del bloqueo duro
```

Queda alineado con la cadena de rotación secuencial existente (`--only`, cooldown, keychain).
El bloqueo real (`AccountLimited`/`Blocked`) sigue siendo la red de seguridad si la
predicción falla.

## Trazabilidad / Grafana

Coherente con la decisión de observabilidad (OTEL + Postgres, **sin Mongo** —
[../06-observability.md](../06-observability.md)):

- **Postgres** (`token_usage` + rollups): paneles SQL nativos en Grafana — consumo por
  cuenta/sesión/modelo, `remaining`, `rate`, `eta_to_block`, ranking de sesiones que más
  queman.
- **Prometheus**: métricas en vivo (`gauge` de remaining, `eta_to_block`, contador de
  `BlockPredicted`/rotaciones predictivas vs reactivas).
- **Traces (Tempo)**: la cadena `TokensSampled → BlockPredicted → Rotated` con `causation_id`
  → "por qué se rotó esta cuenta a esta hora" reconstruible.

### Rollup sugerido

```sql
-- consumo y rate por cuenta en la ventana vigente (para el panel y la predicción)
CREATE MATERIALIZED VIEW account_window_usage AS
SELECT account_id,
       SUM(input_tokens + output_tokens) AS raw_tokens,
       -- coste normalizado se calcula en la capa de dominio, no en SQL
       MIN(ts) AS first_ts, MAX(ts) AS last_ts, COUNT(*) AS samples
FROM token_usage
GROUP BY account_id;
```

## Gate de validación

- Sembrar `token_usage` con un consumo sintético de rate conocido → la predicción calcula un
  `eta_to_block` dentro de tolerancia del valor real.
- Cruzar el umbral en el seed → se emite **un** `BlockPredicted` (idempotente por ventana, no
  spamea) → la rotación dispara antes que cualquier `AccountLimited`.
- **Replay** del log de eventos reconstruye el mismo `eta_to_block` y la misma decisión de
  rotación (prueba que el cálculo es puro/determinista).

## Riesgos / cosas finas

1. **Atribución por sesión es el punto frágil.** El conteo debe llegar etiquetado con el
   `session_id` correcto; ya hay precedente de sesiones huérfanas cuando el env de sesión no
   se setea a nivel de sesión (solo del pane). Sin atribución correcta, el desglose por sesión
   miente aunque el agregado por cuenta sea correcto. **Verificar la atribución antes de
   confiar en el per-session.**
2. **Modelos mezclados.** No sumar tokens de modelos distintos sin normalizar a coste.
3. **Ventanas del proveedor.** El tipo de ventana (rolling vs semanal) y su límite no siempre
   son explícitos; calibrar `window_limit`/`window_kind` contra las cabeceras reales del
   `probe`, no contra supuestos.
4. **Desfase cabeceras ↔ conteo local.** Reconciliar periódicamente; las cabeceras del
   proveedor son la autoridad.
5. **No `read-modify-write` de contadores** si esto tocara Dolt; aquí es Postgres
   (transaccional), pero el agregado va por inserts append-only + rollup, no por UPDATE de un
   contador (evita lost-update).

## Estado

**Núcleo DONE** (Paso 6.c, al 2026-05-27, bead hq-mc72.3). Implementado en
`crates/domain/orchestration/gt-quota` + `crates/kernel/gt-store-pg`:

- `QuotaEvent` con `TokensSampled` / `UsageProbed` / `WindowReset` / `BlockPredicted` /
  `AccountLimited` / `Rotated` / `Blocked`. El reloj viaja como `now_secs` en cada evento.
- `cost::cost_units` normaliza tokens a unidad de coste por modelo (`ModelWeights`); sin
  calibración usa `IDENTITY`.
- `expectations::predict` es **puro**: `consumed`/`remaining`/`rate_per_min`/`eta_to_block`
  + `should_predict_block` (solo dispara si el ETA cae bajo el umbral **y dentro** de la
  ventana vigente). EWMA del rate vive en `AccountRegistry` (actor), idempotencia de
  `BlockPredicted` por ventana.
- `QuotaRepository` (puerto): `token_usage` append-only, sumas por cuenta y por sesión, sin
  `UPDATE` de contadores. `gt-store-pg::PgQuota` lo implementa con `sqlx` (sin macros).
- **Schema versionado con `sqlx::migrate!`.** El DDL inicial (`accounts` + `token_usage` +
  índices) vive en `crates/kernel/gt-store-pg/migrations/20260527000001_init_quota.sql`;
  `ensure_schema(pool)` corre la cadena en cada boot, idempotente, con checksum por
  archivo. La materialized view `account_window_usage` entra como migración nueva cuando
  haya datos reales — los archivos aplicados no se editan.
- Gates verdes: contrato in-memory + Postgres (`GT_PG_URL`), rotación predictiva antes de
  `AccountLimited` con replay byte-idéntico, y reset de ventana que rehabilita la predicción.

**Pendiente:** el `probe.rs` que captura las cabeceras `anthropic-ratelimit-*` reales, el
`keychain` platform-specific (queda detrás del puerto), las métricas Prometheus / traces
Tempo y la materialized view `account_window_usage` (se crea cuando haya datos reales).
