//! `gt-store-pg` — Postgres adapter for the workspace's non-bead state (docs/04).
//!
//! Step 6.c added the `QuotaRepository` (accounts + token_usage). Step 6.f.13 (hq-j9ou.2)
//! adds the canonical audit log (`audit_events`, JSONB payload) — the EventStore lives in
//! Postgres, not Mongo, so Grafana reads it natively. Step 6.h (hq-7owq) finishes the
//! doc-04 §3 outbox: the writer commits the entity + `outbox_events` row in one transaction
//! and a drain task fans every outbox row into `audit_events` + `feed_projections`,
//! mark-drained-after-success. All tables are aligned with the
//! `docs/features/token-tracking-prediction.md` shape and the doc-04 persistence rules:
//! append-heavy, no lost-update, idempotent on the relay key (`event_id`).

mod activity;
mod audit;
mod conn;
mod outbox;
mod quota_repo;
mod rig_repo;
mod schema;

pub use activity::{ActivityEntry, PgActivity};
pub use audit::PgAudit;
pub use conn::connect;
pub use outbox::{PgFeedProjections, PgOutboxDrain, PgOutboxWriter};
pub use quota_repo::PgQuota;
pub use rig_repo::PgRigs;
pub use schema::ensure_schema;
