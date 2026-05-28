use mysql_async::prelude::*;
use mysql_async::{params, Pool, TxOpts};

use gt_events::AppError;
use gt_orchestration::{Convoy, ConvoyState, Member, MemberState, OrchRepository};

use crate::conn::map_err;

/// Dolt adapter for the convoy board (hq-03aw.8 / epic hq-bdn8). Header row in `convoys`
/// + one row per member in `convoy_members`. Patch-style writes (set_member_state /
/// set_convoy_state) avoid rewriting every member on every transition.
pub struct DoltOrch {
    pool: Pool,
}

impl DoltOrch {
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
            "CREATE TABLE IF NOT EXISTS convoys (
                id    VARCHAR(64)  PRIMARY KEY,
                state VARCHAR(32)  NOT NULL
            )",
        )
        .await
        .map_err(map_err)?;
        conn.query_drop(
            "CREATE TABLE IF NOT EXISTS convoy_members (
                convoy   VARCHAR(64)  NOT NULL,
                bead     VARCHAR(64)  NOT NULL,
                position INT          NOT NULL,
                state    VARCHAR(32)  NOT NULL,
                PRIMARY KEY (convoy, bead)
            )",
        )
        .await
        .map_err(map_err)
    }

    pub async fn truncate(&self) -> Result<(), AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        conn.query_drop("DELETE FROM convoy_members").await.map_err(map_err)?;
        conn.query_drop("DELETE FROM convoys").await.map_err(map_err)
    }
}

fn parse_member_state(s: &str) -> MemberState {
    match s {
        "pending" => MemberState::Pending,
        "active" => MemberState::Active,
        "done" => MemberState::Done,
        "failed" => MemberState::Failed,
        _ => MemberState::Failed,
    }
}

fn parse_convoy_state(s: &str) -> ConvoyState {
    match s {
        "staged" => ConvoyState::Staged,
        "launched" => ConvoyState::Launched,
        "closed" => ConvoyState::Closed,
        "failed" => ConvoyState::Failed,
        _ => ConvoyState::Failed,
    }
}

impl OrchRepository for DoltOrch {
    async fn upsert_convoy(&self, convoy: &Convoy) -> Result<(), AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        let mut tx = conn.start_transaction(TxOpts::default()).await.map_err(map_err)?;
        tx.exec_drop(
            "REPLACE INTO convoys (id, state) VALUES (:id, :state)",
            params! { "id" => &convoy.id, "state" => convoy.state.as_str() },
        )
        .await
        .map_err(map_err)?;
        tx.exec_drop(
            "DELETE FROM convoy_members WHERE convoy = :convoy",
            params! { "convoy" => &convoy.id },
        )
        .await
        .map_err(map_err)?;
        for (i, m) in convoy.members.iter().enumerate() {
            tx.exec_drop(
                "INSERT INTO convoy_members (convoy, bead, position, state)
                 VALUES (:convoy, :bead, :position, :state)",
                params! {
                    "convoy" => &convoy.id,
                    "bead" => &m.bead,
                    "position" => i as i32,
                    "state" => m.state.as_str(),
                },
            )
            .await
            .map_err(map_err)?;
        }
        tx.commit().await.map_err(map_err)
    }

    async fn set_member_state(
        &self,
        convoy: &str,
        member: &str,
        state: MemberState,
    ) -> Result<(), AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        conn.exec_drop(
            "UPDATE convoy_members SET state = :state
             WHERE convoy = :convoy AND bead = :bead",
            params! {
                "state" => state.as_str(),
                "convoy" => convoy,
                "bead" => member,
            },
        )
        .await
        .map_err(map_err)
    }

    async fn set_convoy_state(&self, convoy: &str, state: ConvoyState) -> Result<(), AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        conn.exec_drop(
            "UPDATE convoys SET state = :state WHERE id = :id",
            params! { "state" => state.as_str(), "id" => convoy },
        )
        .await
        .map_err(map_err)
    }

    async fn get_convoy(&self, convoy: &str) -> Result<Option<Convoy>, AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        let header: Option<(String, String)> = conn
            .exec_first(
                "SELECT id, state FROM convoys WHERE id = :id",
                params! { "id" => convoy },
            )
            .await
            .map_err(map_err)?;
        let Some((id, state)) = header else { return Ok(None) };
        let members: Vec<(String, String)> = conn
            .exec(
                "SELECT bead, state FROM convoy_members
                 WHERE convoy = :convoy ORDER BY position",
                params! { "convoy" => convoy },
            )
            .await
            .map_err(map_err)?;
        Ok(Some(Convoy {
            id,
            state: parse_convoy_state(&state),
            members: members
                .into_iter()
                .map(|(bead, st)| Member { bead, state: parse_member_state(&st) })
                .collect(),
        }))
    }

    async fn list_convoys(&self) -> Result<Vec<Convoy>, AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        let headers: Vec<(String, String)> = conn
            .query("SELECT id, state FROM convoys ORDER BY id")
            .await
            .map_err(map_err)?;
        let mut out = Vec::with_capacity(headers.len());
        for (id, state) in headers {
            let members: Vec<(String, String)> = conn
                .exec(
                    "SELECT bead, state FROM convoy_members
                     WHERE convoy = :convoy ORDER BY position",
                    params! { "convoy" => &id },
                )
                .await
                .map_err(map_err)?;
            out.push(Convoy {
                id,
                state: parse_convoy_state(&state),
                members: members
                    .into_iter()
                    .map(|(bead, st)| Member { bead, state: parse_member_state(&st) })
                    .collect(),
            });
        }
        Ok(out)
    }
}
