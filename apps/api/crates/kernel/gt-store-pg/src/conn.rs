use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

use gt_events::AppError;

/// Create a connection pool to Postgres. `url` e.g.
/// `postgres://gt:gt@127.0.0.1:5432/gt_rs_test`.
pub async fn connect(url: &str) -> Result<PgPool, AppError> {
    PgPoolOptions::new()
        .max_connections(8)
        .connect(url)
        .await
        .map_err(|e| AppError::Other(format!("pg connect: {e}")))
}

/// Map a sqlx error to `AppError` (I/O errors live at the edges and cross the domain
/// boundary here).
pub(crate) fn map_err(e: impl std::fmt::Display) -> AppError {
    AppError::Other(format!("pg: {e}"))
}
