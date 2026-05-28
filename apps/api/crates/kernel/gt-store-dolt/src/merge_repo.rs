use mysql_async::prelude::*;
use mysql_async::{params, Pool};

use gt_events::AppError;
use gt_merge::{MergeRepository, MergeSlot, MergeSlotState};

use crate::conn::map_err;

/// Dolt adapter for the merge-board persistence port (hq-03aw.6 / epic hq-bdn8). One row
/// per slot; state-machine snapshot only — the audit log + replay reducer are still the
/// source of truth.
pub struct DoltMerge {
    pool: Pool,
}

impl DoltMerge {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    pub fn connect(url: &str) -> Result<Self, AppError> {
        Ok(Self::new(crate::conn::connect(url)?))
    }

    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    /// Create the `merge_slots` table if it doesn't exist.
    pub async fn ensure_schema(&self) -> Result<(), AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        conn.query_drop(
            "CREATE TABLE IF NOT EXISTS merge_slots (
                bead   VARCHAR(64)  PRIMARY KEY,
                branch TEXT         NOT NULL,
                state  VARCHAR(32)  NOT NULL
            )",
        )
        .await
        .map_err(map_err)
    }

    /// Clear the table (tests / slate limpio).
    pub async fn truncate(&self) -> Result<(), AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        conn.query_drop("DELETE FROM merge_slots")
            .await
            .map_err(map_err)
    }
}

fn parse_state(s: &str) -> MergeSlotState {
    match s {
        "ready" => MergeSlotState::Ready,
        "merging" => MergeSlotState::Merging,
        "merged" => MergeSlotState::Merged,
        "failed" => MergeSlotState::Failed,
        _ => MergeSlotState::Failed,
    }
}

fn row_to_slot((bead, branch, state): (String, String, String)) -> MergeSlot {
    MergeSlot {
        bead,
        branch,
        state: parse_state(&state),
    }
}

impl MergeRepository for DoltMerge {
    async fn upsert_slot(&self, slot: &MergeSlot) -> Result<(), AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        conn.exec_drop(
            "REPLACE INTO merge_slots (bead, branch, state)
             VALUES (:bead, :branch, :state)",
            params! {
                "bead" => &slot.bead,
                "branch" => &slot.branch,
                "state" => slot.state.as_str(),
            },
        )
        .await
        .map_err(map_err)
    }

    async fn get_slot(&self, bead: &str) -> Result<Option<MergeSlot>, AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        let row: Option<(String, String, String)> = conn
            .exec_first(
                "SELECT bead, branch, state FROM merge_slots WHERE bead = :bead",
                params! { "bead" => bead },
            )
            .await
            .map_err(map_err)?;
        Ok(row.map(row_to_slot))
    }

    async fn list_slots(&self) -> Result<Vec<MergeSlot>, AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        let rows: Vec<(String, String, String)> = conn
            .query("SELECT bead, branch, state FROM merge_slots ORDER BY bead")
            .await
            .map_err(map_err)?;
        Ok(rows.into_iter().map(row_to_slot).collect())
    }
}
