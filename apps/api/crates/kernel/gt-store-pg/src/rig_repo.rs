use sqlx::postgres::PgRow;
use sqlx::PgPool;
use sqlx::Row;

use gt_events::AppError;
use gt_rig::{RigEntry, RigRepository};

use crate::conn::map_err;

/// Postgres adapter for the `RigRepository` port. The `rigs` table's UNIQUE index on `prefix`
/// mirrors the catalog's prefix-collision invariant.
pub struct PgRigs {
    pool: PgPool,
}

impl PgRigs {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn connect(url: &str) -> Result<Self, AppError> {
        Ok(Self::new(crate::conn::connect(url).await?))
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Useful for tests / a clean slate.
    pub async fn truncate(&self) -> Result<(), AppError> {
        sqlx::query("TRUNCATE TABLE rigs")
            .execute(&self.pool)
            .await
            .map_err(map_err)?;
        Ok(())
    }
}

const SELECT_COLS: &str =
    "name, prefix, git_url, push_url, upstream_url, default_branch, registered_at";

fn entry_from_row(r: &PgRow) -> Result<RigEntry, AppError> {
    let registered_at: i64 = r.try_get("registered_at").map_err(map_err)?;
    Ok(RigEntry {
        name: r.try_get("name").map_err(map_err)?,
        prefix: r.try_get("prefix").map_err(map_err)?,
        git_url: r.try_get("git_url").map_err(map_err)?,
        push_url: r.try_get("push_url").map_err(map_err)?,
        upstream_url: r.try_get("upstream_url").map_err(map_err)?,
        default_branch: r.try_get("default_branch").map_err(map_err)?,
        registered_at_secs: registered_at.max(0) as u64,
    })
}

impl RigRepository for PgRigs {
    async fn upsert(&self, entry: &RigEntry) -> Result<(), AppError> {
        sqlx::query(
            "INSERT INTO rigs
               (name, prefix, git_url, push_url, upstream_url, default_branch, registered_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (name) DO UPDATE
               SET prefix = EXCLUDED.prefix,
                   git_url = EXCLUDED.git_url,
                   push_url = EXCLUDED.push_url,
                   upstream_url = EXCLUDED.upstream_url,
                   default_branch = EXCLUDED.default_branch,
                   registered_at = EXCLUDED.registered_at",
        )
        .bind(&entry.name)
        .bind(&entry.prefix)
        .bind(&entry.git_url)
        .bind(&entry.push_url)
        .bind(&entry.upstream_url)
        .bind(&entry.default_branch)
        .bind(entry.registered_at_secs as i64)
        .execute(&self.pool)
        .await
        .map_err(map_err)?;
        Ok(())
    }

    async fn remove(&self, name: &str) -> Result<bool, AppError> {
        let res = sqlx::query("DELETE FROM rigs WHERE name = $1")
            .bind(name)
            .execute(&self.pool)
            .await
            .map_err(map_err)?;
        Ok(res.rows_affected() > 0)
    }

    async fn get(&self, name: &str) -> Result<Option<RigEntry>, AppError> {
        let row = sqlx::query(&format!("SELECT {SELECT_COLS} FROM rigs WHERE name = $1"))
            .bind(name)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_err)?;
        row.as_ref().map(entry_from_row).transpose()
    }

    async fn prefix_owner(&self, prefix: &str) -> Result<Option<String>, AppError> {
        let row = sqlx::query("SELECT name FROM rigs WHERE prefix = $1")
            .bind(prefix)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_err)?;
        match row {
            Some(r) => Ok(Some(r.try_get("name").map_err(map_err)?)),
            None => Ok(None),
        }
    }

    async fn list(&self) -> Result<Vec<RigEntry>, AppError> {
        let rows = sqlx::query(&format!("SELECT {SELECT_COLS} FROM rigs ORDER BY name"))
            .fetch_all(&self.pool)
            .await
            .map_err(map_err)?;
        rows.iter().map(entry_from_row).collect()
    }
}
