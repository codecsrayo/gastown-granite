use mysql_async::prelude::*;
use mysql_async::{params, Pool};

use gt_beads::{Bead, BeadRepository, BeadStatus};
use gt_events::AppError;

use crate::conn::map_err;

/// Adaptador Dolt del puerto `BeadRepository`.
pub struct DoltBeads {
    pool: Pool,
}

impl DoltBeads {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Conecta y devuelve el adaptador.
    pub fn connect(url: &str) -> Result<Self, AppError> {
        Ok(Self::new(crate::conn::connect(url)?))
    }

    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    /// Crea la tabla `beads` si no existe (migración mínima del Paso 4).
    pub async fn ensure_schema(&self) -> Result<(), AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        conn.query_drop(
            "CREATE TABLE IF NOT EXISTS beads (
                id        VARCHAR(64)  PRIMARY KEY,
                title     TEXT         NOT NULL,
                status    VARCHAR(32)  NOT NULL,
                priority  INT          NOT NULL,
                assignee  VARCHAR(128) NULL
            )",
        )
        .await
        .map_err(map_err)
    }

    /// Vacía la tabla (solo para tests / slate limpio).
    pub async fn truncate(&self) -> Result<(), AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        conn.query_drop("DELETE FROM beads").await.map_err(map_err)
    }
}

fn row_to_bead((id, title, status, priority, assignee): (String, String, String, i32, Option<String>)) -> Bead {
    Bead {
        id,
        title,
        status: BeadStatus::parse(&status).unwrap_or(BeadStatus::Failed),
        priority: priority as u8,
        assignee,
    }
}

impl BeadRepository for DoltBeads {
    async fn upsert(&self, bead: &Bead) -> Result<(), AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        conn.exec_drop(
            "REPLACE INTO beads (id, title, status, priority, assignee)
             VALUES (:id, :title, :status, :priority, :assignee)",
            params! {
                "id" => &bead.id,
                "title" => &bead.title,
                "status" => bead.status.as_str(),
                "priority" => bead.priority as i32,
                "assignee" => &bead.assignee,
            },
        )
        .await
        .map_err(map_err)
    }

    async fn get(&self, id: &str) -> Result<Option<Bead>, AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        let row: Option<(String, String, String, i32, Option<String>)> = conn
            .exec_first(
                "SELECT id, title, status, priority, assignee FROM beads WHERE id = :id",
                params! { "id" => id },
            )
            .await
            .map_err(map_err)?;
        Ok(row.map(row_to_bead))
    }

    async fn list_by_status(&self, status: BeadStatus) -> Result<Vec<Bead>, AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        let rows: Vec<(String, String, String, i32, Option<String>)> = conn
            .exec(
                "SELECT id, title, status, priority, assignee FROM beads WHERE status = :s",
                params! { "s" => status.as_str() },
            )
            .await
            .map_err(map_err)?;
        Ok(rows.into_iter().map(row_to_bead).collect())
    }

    async fn cas_claim(&self, id: &str, worker: &str) -> Result<bool, AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        // Gana quien encuentra el bead aún 'pending'. affected_rows == 1 → es nuestro.
        conn.exec_drop(
            "UPDATE beads SET status = 'dispatched', assignee = :w
             WHERE id = :id AND status = 'pending'",
            params! { "w" => worker, "id" => id },
        )
        .await
        .map_err(map_err)?;
        Ok(conn.affected_rows() == 1)
    }

    async fn cas_release(&self, id: &str, expected_worker: &str) -> Result<bool, AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        // Solo libera leases vivos cuyo dueño coincide (otro patrol o un completion ya
        // movió el bead → affected_rows == 0; el caller no re-encola).
        conn.exec_drop(
            "UPDATE beads SET status = 'pending', assignee = NULL
             WHERE id = :id AND status = 'dispatched' AND assignee = :w",
            params! { "id" => id, "w" => expected_worker },
        )
        .await
        .map_err(map_err)?;
        Ok(conn.affected_rows() == 1)
    }
}
