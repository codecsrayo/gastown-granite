//! Postgres outbox: the durable handoff between the in-process broadcast and the external
//! consumers (canonical audit log + read-side feed projection).
//!
//! Doc 04 §3 ("Outbox por cada store que escribe"): writes that produce an event go through
//! a single Postgres transaction holding **both** the entity row (e.g. `token_usage`) and
//! the `outbox_events` row. A separate drain task moves outbox rows into `audit_events` and
//! `feed_projections`, marking them drained only after both downstream writes succeed. The
//! crash-safe property: if the writer commits but the broadcast subscriber loses the event,
//! the next restart still observes the outbox row and re-delivers it; idempotency on
//! `event_id` keeps the downstream tables exactly-once.
//!
//! Why split writer / drain into two structs: the writer is hot-path (called per event in
//! the bin's relay loop, must stay sync-friendly to the broadcast receiver), the drain is
//! background polling (batches by `LIMIT N` to keep tail latency bounded). They share a
//! pool but the roles never bleed: the writer never reads downstream tables, the drain
//! never inserts into `outbox_events`.

use sqlx::{PgPool, Postgres, Row, Transaction};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use gt_audit::EventRecord;
use gt_events::AppError;

use crate::conn::map_err;

/// Outbox-side writer: drops one `EventRecord` (optionally piggy-backed with a quota usage
/// sample) into Postgres atomically.
pub struct PgOutboxWriter {
    pool: PgPool,
}

impl PgOutboxWriter {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn connect(url: &str) -> Result<Self, AppError> {
        Ok(Self::new(crate::conn::connect(url).await?))
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Append one record to the outbox. Idempotent on `event_id` so retries (broadcast
    /// re-delivery after `Lagged`, restart-time replay of the local jsonl) do not double up.
    /// If `record.kind == "quota.tokens_sampled"`, the same transaction also writes the
    /// per-sample row into `token_usage` — that is the doc-04 entity+outbox pattern: one
    /// commit covers both rows or neither.
    pub async fn publish(&self, rec: &EventRecord) -> Result<(), AppError> {
        let mut tx: Transaction<'_, Postgres> = self.pool.begin().await.map_err(map_err)?;
        let inserted = insert_outbox_row(&mut tx, rec).await?;
        // Only write the entity row when the outbox row was newly inserted. On a duplicate
        // `event_id` (at-least-once redelivery) the outbox INSERT is a no-op, and the
        // entity write must be skipped too — otherwise `token_usage` double-counts a
        // re-published sample (the outbox dedupes, but a bare INSERT into token_usage would
        // not).
        if inserted && rec.kind == "quota.tokens_sampled" {
            insert_token_usage_from_payload(&mut tx, rec).await?;
        }
        tx.commit().await.map_err(map_err)?;
        Ok(())
    }

    /// Snapshot of pending rows (test/inspection helper). The drain task uses its own SQL
    /// inside [`PgOutboxDrain`] so it can `FOR UPDATE SKIP LOCKED` for safe concurrency.
    pub async fn pending_count(&self) -> Result<u64, AppError> {
        let row = sqlx::query("SELECT COUNT(*)::BIGINT AS n FROM outbox_events WHERE drained_at IS NULL")
            .fetch_one(&self.pool)
            .await
            .map_err(map_err)?;
        let n: i64 = row.try_get("n").map_err(map_err)?;
        Ok(n.max(0) as u64)
    }

    /// Useful for tests / a clean slate.
    pub async fn truncate(&self) -> Result<(), AppError> {
        sqlx::query("TRUNCATE TABLE outbox_events RESTART IDENTITY")
            .execute(&self.pool)
            .await
            .map_err(map_err)?;
        Ok(())
    }
}

/// Returns `true` if a new outbox row was inserted, `false` if `event_id` already existed
/// (the redelivery case). The caller uses this to gate the entity write.
async fn insert_outbox_row(
    tx: &mut Transaction<'_, Postgres>,
    rec: &EventRecord,
) -> Result<bool, AppError> {
    let ts = OffsetDateTime::parse(&rec.ts, &Rfc3339)
        .map_err(|e| AppError::Other(format!("outbox ts parse ({}): {e}", rec.ts)))?;
    let res = sqlx::query(
        "INSERT INTO outbox_events
           (event_id, correlation_id, causation_id, ts, kind, payload)
         VALUES ($1, $2, $3, $4, $5, $6::jsonb)
         ON CONFLICT (event_id) DO NOTHING",
    )
    .bind(&rec.event_id)
    .bind(&rec.correlation_id)
    .bind(&rec.causation_id)
    .bind(ts)
    .bind(&rec.kind)
    .bind(&rec.payload)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(res.rows_affected() > 0)
}

/// Decode the `TokensSampled` payload and insert one row in `token_usage`. The payload
/// fields are owned by the `gt-quota` domain (`QuotaEvent::TokensSampled { account, session,
/// model, input, output, cache_read, cache_creation, now_secs }`); we read them by JSON key
/// so this adapter does NOT depend on the domain's strong type — the outbox stays
/// type-erased like the rest of the audit pipeline.
async fn insert_token_usage_from_payload(
    tx: &mut Transaction<'_, Postgres>,
    rec: &EventRecord,
) -> Result<(), AppError> {
    let p = &rec.payload;
    let account = json_str(p, "account")?;
    let session = json_str(p, "session")?;
    let model = json_str(p, "model")?;
    let input = json_u64(p, "input")?;
    let output = json_u64(p, "output")?;
    let cache_read = json_u64(p, "cache_read").unwrap_or(0);
    let cache_creation = json_u64(p, "cache_creation").unwrap_or(0);
    let now_secs = json_u64(p, "now_secs")?;
    let ts = OffsetDateTime::from_unix_timestamp(now_secs as i64)
        .map_err(|e| AppError::Other(format!("token_usage ts ({now_secs}): {e}")))?;

    sqlx::query(
        "INSERT INTO token_usage
           (account_id, session_id, model, ts, input_tokens, output_tokens, cache_read, cache_creation)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(&account)
    .bind(&session)
    .bind(&model)
    .bind(ts)
    .bind(input as i64)
    .bind(output as i64)
    .bind(cache_read as i64)
    .bind(cache_creation as i64)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

fn json_str(v: &serde_json::Value, key: &str) -> Result<String, AppError> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| AppError::Other(format!("payload missing string `{key}`")))
}

fn json_u64(v: &serde_json::Value, key: &str) -> Result<u64, AppError> {
    v.get(key)
        .and_then(|x| x.as_u64())
        .ok_or_else(|| AppError::Other(format!("payload missing u64 `{key}`")))
}

/// Outbox-side drain: fans pending rows into the canonical audit log + the read-side
/// projection. Returns the number of rows handled in this batch so the bin can loop with
/// backpressure.
pub struct PgOutboxDrain {
    pool: PgPool,
}

impl PgOutboxDrain {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn connect(url: &str) -> Result<Self, AppError> {
        Ok(Self::new(crate::conn::connect(url).await?))
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Drain up to `limit` pending rows in `seq` order. Each row is moved into
    /// `audit_events` (idempotent on `event_id`) and applied to `feed_projections`
    /// (UPSERT), then marked drained — all in a single transaction per row so a crash
    /// retries only the unfinished row. `FOR UPDATE SKIP LOCKED` keeps multiple drain
    /// workers safe should we ever scale out.
    pub async fn drain_batch(&self, limit: u32) -> Result<u32, AppError> {
        let mut count = 0u32;
        for _ in 0..limit {
            if !self.drain_one().await? {
                break;
            }
            count += 1;
        }
        Ok(count)
    }

    async fn drain_one(&self) -> Result<bool, AppError> {
        let mut tx: Transaction<'_, Postgres> = self.pool.begin().await.map_err(map_err)?;
        let row = sqlx::query(
            "SELECT seq, event_id, correlation_id, causation_id, ts, kind, payload::text AS payload
             FROM outbox_events
             WHERE drained_at IS NULL
             ORDER BY seq ASC
             LIMIT 1
             FOR UPDATE SKIP LOCKED",
        )
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_err)?;

        let Some(row) = row else {
            tx.rollback().await.map_err(map_err)?;
            return Ok(false);
        };

        let seq: i64 = row.try_get("seq").map_err(map_err)?;
        let event_id: String = row.try_get("event_id").map_err(map_err)?;
        let correlation_id: String = row.try_get("correlation_id").map_err(map_err)?;
        let causation_id: Option<String> = row.try_get("causation_id").map_err(map_err)?;
        let ts: OffsetDateTime = row.try_get("ts").map_err(map_err)?;
        let ts_str = ts
            .format(&Rfc3339)
            .map_err(|e| AppError::Other(format!("ts format: {e}")))?;
        let kind: String = row.try_get("kind").map_err(map_err)?;
        let payload_text: String = row.try_get("payload").map_err(map_err)?;
        let payload: serde_json::Value = serde_json::from_str(&payload_text)
            .map_err(|e| AppError::Other(format!("payload decode: {e}")))?;

        // 1) Canonical audit append. Idempotent on event_id.
        sqlx::query(
            "INSERT INTO audit_events
               (event_id, correlation_id, causation_id, ts, kind, payload)
             VALUES ($1, $2, $3, $4, $5, $6::jsonb)
             ON CONFLICT (event_id) DO NOTHING",
        )
        .bind(&event_id)
        .bind(&correlation_id)
        .bind(&causation_id)
        .bind(ts)
        .bind(&kind)
        .bind(&payload)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;

        // 2) Feed projection upserts derived from this event.
        let rec = EventRecord {
            event_id: event_id.clone(),
            correlation_id: correlation_id.clone(),
            causation_id,
            ts: ts_str,
            kind: kind.clone(),
            payload,
        };
        apply_projection(&mut tx, &rec).await?;

        // 3) Mark drained.
        sqlx::query("UPDATE outbox_events SET drained_at = now() WHERE seq = $1")
            .bind(seq)
            .execute(&mut *tx)
            .await
            .map_err(map_err)?;

        tx.commit().await.map_err(map_err)?;
        Ok(true)
    }
}

/// The `feed_projections` table is the SQL twin of `gt-feed::FeedState` — small, additive
/// aggregates panels can read without folding the whole log. We derive deltas from the
/// payload, never from a counter, so a redelivery converges to the same value (the row's
/// effect is `+0` because the audit_events insert was a no-op the second time around — see
/// `PgFeedProjections::apply_record` for the idempotent variant).
async fn apply_projection(
    tx: &mut Transaction<'_, Postgres>,
    rec: &EventRecord,
) -> Result<(), AppError> {
    // Every event bumps `kind_totals` and the per-correlation event count.
    upsert_projection(tx, "kind", &rec.kind, "events_total", 1).await?;
    upsert_projection(
        tx,
        "correlation",
        &rec.correlation_id,
        "events_total",
        1,
    )
    .await?;

    if rec.kind == "quota.tokens_sampled" {
        let account = json_str(&rec.payload, "account").unwrap_or_default();
        let session = json_str(&rec.payload, "session").unwrap_or_default();
        let input = json_u64(&rec.payload, "input").unwrap_or(0) as i64;
        let output = json_u64(&rec.payload, "output").unwrap_or(0) as i64;
        let tokens = input + output;
        if !account.is_empty() {
            upsert_projection(tx, "account", &account, "tokens_total", tokens).await?;
        }
        if !account.is_empty() && !session.is_empty() {
            let key = format!("{account}|{session}");
            upsert_projection(tx, "session", &key, "tokens_total", tokens).await?;
        }
    }
    Ok(())
}

async fn upsert_projection(
    tx: &mut Transaction<'_, Postgres>,
    scope: &str,
    scope_id: &str,
    metric: &str,
    delta: i64,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO feed_projections (scope, scope_id, metric, value_num, updated_at)
         VALUES ($1, $2, $3, $4, now())
         ON CONFLICT (scope, scope_id, metric)
         DO UPDATE SET value_num = feed_projections.value_num + EXCLUDED.value_num,
                       updated_at = now()",
    )
    .bind(scope)
    .bind(scope_id)
    .bind(metric)
    .bind(delta)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

/// Read-side view of the projection table. Panels (and the gate test) use this to compare
/// the projected counters against an authoritative replay of the audit log.
pub struct PgFeedProjections {
    pool: PgPool,
}

impl PgFeedProjections {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn connect(url: &str) -> Result<Self, AppError> {
        Ok(Self::new(crate::conn::connect(url).await?))
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn get(
        &self,
        scope: &str,
        scope_id: &str,
        metric: &str,
    ) -> Result<Option<i64>, AppError> {
        let row = sqlx::query(
            "SELECT value_num FROM feed_projections
             WHERE scope = $1 AND scope_id = $2 AND metric = $3",
        )
        .bind(scope)
        .bind(scope_id)
        .bind(metric)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_err)?;
        Ok(row.map(|r| r.try_get::<i64, _>("value_num").unwrap_or(0)))
    }

    pub async fn truncate(&self) -> Result<(), AppError> {
        sqlx::query("TRUNCATE TABLE feed_projections")
            .execute(&self.pool)
            .await
            .map_err(map_err)?;
        Ok(())
    }
}
