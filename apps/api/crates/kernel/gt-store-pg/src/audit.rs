//! Postgres adapter for the canonical audit log (docs/04-persistence.md).
//!
//! `gt-audit::EventStore` is sync (the in-process JSONL writer is the reference impl); this
//! adapter intentionally exposes an **async** API instead — Postgres lives on the async edge
//! and the bin drains the bus into here from a dedicated relay task, so an async signature
//! matches the call site and avoids a `block_on` inside the reactor.
//!
//! The `payload` column is `JSONB`: Grafana/SQL query the type-erased domain payload
//! directly. `event_id` is `PRIMARY KEY` + `ON CONFLICT DO NOTHING`, so at-least-once relays
//! land exactly once (docs/04 §Idempotencia).

use sqlx::PgPool;
use sqlx::Row;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use gt_audit::EventRecord;
use gt_events::AppError;

use crate::conn::map_err;

/// Postgres adapter for the `audit_events` table.
pub struct PgAudit {
    pool: PgPool,
}

impl PgAudit {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn connect(url: &str) -> Result<Self, AppError> {
        Ok(Self::new(crate::conn::connect(url).await?))
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Useful for tests / a clean slate.
    pub async fn truncate(&self) -> Result<(), AppError> {
        sqlx::query("TRUNCATE TABLE audit_events")
            .execute(&self.pool)
            .await
            .map_err(map_err)?;
        Ok(())
    }

    /// Append one record. Idempotent on `event_id` so an at-least-once relay can replay
    /// without producing duplicates in the audit.
    pub async fn append(&self, rec: &EventRecord) -> Result<(), AppError> {
        let ts = OffsetDateTime::parse(&rec.ts, &Rfc3339)
            .map_err(|e| AppError::Other(format!("ts parse ({}): {e}", rec.ts)))?;
        sqlx::query(
            "INSERT INTO audit_events
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
        .execute(&self.pool)
        .await
        .map_err(map_err)?;
        Ok(())
    }

    /// Read every audit row back as `EventRecord`s in time order. Used by the contract test
    /// to assert byte-identical round-trip and by replay tooling that prefers SQL over the
    /// local jsonl fallback.
    pub async fn read_all(&self) -> Result<Vec<EventRecord>, AppError> {
        let rows = sqlx::query(
            "SELECT event_id, correlation_id, causation_id, ts, kind, payload::text AS payload
             FROM audit_events
             ORDER BY ts ASC, event_id ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(map_err)?;
        rows.into_iter()
            .map(|r| {
                let event_id: String = r.try_get("event_id").map_err(map_err)?;
                let correlation_id: String = r.try_get("correlation_id").map_err(map_err)?;
                let causation_id: Option<String> = r.try_get("causation_id").map_err(map_err)?;
                let ts: OffsetDateTime = r.try_get("ts").map_err(map_err)?;
                let ts = ts
                    .format(&Rfc3339)
                    .map_err(|e| AppError::Other(format!("ts format: {e}")))?;
                let kind: String = r.try_get("kind").map_err(map_err)?;
                let payload_text: String = r.try_get("payload").map_err(map_err)?;
                let payload: serde_json::Value = serde_json::from_str(&payload_text)
                    .map_err(|e| AppError::Other(format!("payload decode: {e}")))?;
                Ok(EventRecord {
                    event_id,
                    correlation_id,
                    causation_id,
                    ts,
                    kind,
                    payload,
                })
            })
            .collect()
    }
}
