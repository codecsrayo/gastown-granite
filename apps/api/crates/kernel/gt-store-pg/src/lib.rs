//! `gt-store-pg` — adaptador Postgres: implementa `AccountRepository` (gt-quota), el
//! `EventStore` de audit en columna **JSONB** (sin Mongo), las proyecciones de feed, y el
//! patrón `outbox` (ver `docs/04-persistence.md`).
//!
//! Skeleton (Paso 0): se implementa con `gt-quota` (Paso 6+), con sqlx/SeaORM.

// TODO(paso 6+): pool, quota_repo (AccountRepository), audit_store (EventStore, JSONB),
// feed_proj, outbox relay.
