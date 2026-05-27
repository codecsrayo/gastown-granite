use sqlx::PgPool;

use gt_events::AppError;

use crate::conn::map_err;

/// Minimal Step 6.c migration: `accounts` + `token_usage` + indexes.
///
/// `token_usage` is heavy-write append-only; the `(account_id, ts)` and `(session_id, ts)`
/// indexes cover the panel and the rollup. The spec's materialized view
/// (`account_window_usage`) is created separately once there is real data — here we only
/// ensure the base tables and the indexes.
pub async fn ensure_schema(pool: &PgPool) -> Result<(), AppError> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS accounts (
            id       TEXT PRIMARY KEY,
            status   TEXT NOT NULL,
            window   JSONB NULL
        )",
    )
    .execute(pool)
    .await
    .map_err(map_err)?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS token_usage (
            id              BIGSERIAL PRIMARY KEY,
            account_id      TEXT        NOT NULL,
            session_id      TEXT        NOT NULL,
            model           TEXT        NOT NULL,
            ts              TIMESTAMPTZ NOT NULL,
            input_tokens    BIGINT      NOT NULL,
            output_tokens   BIGINT      NOT NULL,
            cache_read      BIGINT      NOT NULL DEFAULT 0,
            cache_creation  BIGINT      NOT NULL DEFAULT 0
        )",
    )
    .execute(pool)
    .await
    .map_err(map_err)?;

    sqlx::query("CREATE INDEX IF NOT EXISTS token_usage_account_ts ON token_usage (account_id, ts)")
        .execute(pool)
        .await
        .map_err(map_err)?;
    sqlx::query("CREATE INDEX IF NOT EXISTS token_usage_session_ts ON token_usage (session_id, ts)")
        .execute(pool)
        .await
        .map_err(map_err)?;

    Ok(())
}
