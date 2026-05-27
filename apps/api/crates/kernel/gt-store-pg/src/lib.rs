//! `gt-store-pg` — Postgres adapter for the `QuotaRepository` port (gt-quota).
//!
//! Step 6.c (`docs/08-getting-started.md`). The workspace's first Postgres, aligned with
//! `docs/04-persistence.md` and `docs/features/token-tracking-prediction.md`. `token_usage`
//! is append-only: the rate and the prediction are computed over the samples + the account
//! state, never by an `UPDATE` of a counter (avoids lost-update).
//!
//! TODO(step 6+): `audit_store` in JSONB (no Mongo), feed projections and the outbox relay.

mod conn;
mod quota_repo;
mod schema;

pub use conn::connect;
pub use quota_repo::PgQuota;
pub use schema::ensure_schema;
