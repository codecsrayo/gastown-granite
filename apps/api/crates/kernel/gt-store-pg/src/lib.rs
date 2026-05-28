//! `gt-store-pg` — Postgres adapter for the workspace's non-bead state (docs/04).
//!
//! Step 6.c added the `QuotaRepository` (accounts + token_usage). Step 6.f.13 (hq-j9ou.2)
//! adds the canonical audit log (`audit_events`, JSONB payload) — the EventStore lives in
//! Postgres, not Mongo, so Grafana reads it natively. Both tables are aligned with the
//! `docs/features/token-tracking-prediction.md` shape and the doc-04 persistence rules:
//! append-heavy, no lost-update, idempotent on the relay key (`event_id`).

mod audit;
mod conn;
mod quota_repo;
mod schema;

pub use audit::PgAudit;
pub use conn::connect;
pub use quota_repo::PgQuota;
pub use schema::ensure_schema;
