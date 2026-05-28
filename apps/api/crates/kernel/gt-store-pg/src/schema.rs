use sqlx::PgPool;

use gt_events::AppError;

use crate::conn::map_err;

/// Apply pending migrations from `./migrations` (versioned, append-only).
///
/// `sqlx::migrate!` embeds the `.sql` files at compile time by reading only the directory
/// — no live DB at build-time, consistent with the runtime-`query` (no macros) choice.
/// At runtime it tracks applied versions in `_sqlx_migrations` and skips the ones already
/// run, so calling this on every boot is idempotent. Each migration is checksum-validated:
/// never edit an applied file, add a new one.
///
/// The spec's materialized view (`account_window_usage`) lands in a later migration once
/// there is real data.
pub async fn ensure_schema(pool: &PgPool) -> Result<(), AppError> {
    sqlx::migrate!("./migrations")
        .run(pool)
        .await
        .map_err(map_err)
}
