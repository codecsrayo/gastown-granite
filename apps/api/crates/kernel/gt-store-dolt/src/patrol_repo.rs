use mysql_async::prelude::*;
use mysql_async::{params, Pool};

use gt_events::AppError;
use gt_patrol::{Lease, PatrolRepository};

use crate::conn::map_err;

/// Dolt adapter for the patrol lease tracker (hq-03aw.7 / epic hq-bdn8). One row per live
/// lease; deleted on Close / LeaseExpired. Snapshot only — replay still reconstructs the
/// canonical state from the audit log.
pub struct DoltPatrol {
    pool: Pool,
}

impl DoltPatrol {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    pub fn connect(url: &str) -> Result<Self, AppError> {
        Ok(Self::new(crate::conn::connect(url)?))
    }

    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    pub async fn ensure_schema(&self) -> Result<(), AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        conn.query_drop(
            "CREATE TABLE IF NOT EXISTS patrol_leases (
                bead           VARCHAR(64)  PRIMARY KEY,
                worker         VARCHAR(128) NOT NULL,
                priority       INT          NOT NULL,
                last_seen_secs BIGINT       NOT NULL
            )",
        )
        .await
        .map_err(map_err)
    }

    pub async fn truncate(&self) -> Result<(), AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        conn.query_drop("DELETE FROM patrol_leases")
            .await
            .map_err(map_err)
    }
}

fn row_to_lease((bead, worker, priority, last_seen_secs): (String, String, i32, i64)) -> Lease {
    Lease {
        bead,
        worker,
        priority: priority as u8,
        last_seen_secs: last_seen_secs as u64,
    }
}

impl PatrolRepository for DoltPatrol {
    async fn upsert_lease(&self, lease: &Lease) -> Result<(), AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        conn.exec_drop(
            "REPLACE INTO patrol_leases (bead, worker, priority, last_seen_secs)
             VALUES (:bead, :worker, :priority, :last_seen_secs)",
            params! {
                "bead" => &lease.bead,
                "worker" => &lease.worker,
                "priority" => lease.priority as i32,
                "last_seen_secs" => lease.last_seen_secs as i64,
            },
        )
        .await
        .map_err(map_err)
    }

    async fn delete_lease(&self, bead: &str) -> Result<(), AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        conn.exec_drop(
            "DELETE FROM patrol_leases WHERE bead = :bead",
            params! { "bead" => bead },
        )
        .await
        .map_err(map_err)
    }

    async fn heartbeat_worker(&self, worker: &str, now_secs: u64) -> Result<usize, AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        conn.exec_drop(
            "UPDATE patrol_leases SET last_seen_secs = :now WHERE worker = :worker",
            params! { "now" => now_secs as i64, "worker" => worker },
        )
        .await
        .map_err(map_err)?;
        Ok(conn.affected_rows() as usize)
    }

    async fn get_lease(&self, bead: &str) -> Result<Option<Lease>, AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        let row: Option<(String, String, i32, i64)> = conn
            .exec_first(
                "SELECT bead, worker, priority, last_seen_secs
                 FROM patrol_leases WHERE bead = :bead",
                params! { "bead" => bead },
            )
            .await
            .map_err(map_err)?;
        Ok(row.map(row_to_lease))
    }

    async fn list_leases(&self) -> Result<Vec<Lease>, AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        let rows: Vec<(String, String, i32, i64)> = conn
            .query(
                "SELECT bead, worker, priority, last_seen_secs
                 FROM patrol_leases ORDER BY bead",
            )
            .await
            .map_err(map_err)?;
        Ok(rows.into_iter().map(row_to_lease).collect())
    }
}
