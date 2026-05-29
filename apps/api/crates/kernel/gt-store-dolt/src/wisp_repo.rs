use mysql_async::prelude::*;
use mysql_async::{params, Pool};
use time::OffsetDateTime;

use gt_events::AppError;
use gt_wisp::{Wisp, WispKind, WispRepository, WispStatus};

use crate::conn::map_err;

/// Dolt adapter for the wisp compaction port (hq-t9vt / paso 9.C). One row per wisp in the
/// `wisps` table; the reaper reads open/closed snapshots and performs the `Open → Closed`
/// (reap) and row-delete (purge) mutations.
///
/// Timestamps round-trip as Unix epoch seconds via `UNIX_TIMESTAMP` / `FROM_UNIXTIME` rather
/// than a Rust `DATETIME` mapping — `mysql_async` is built with `minimal` features here, so
/// it has no `time` value conversions. Epochs assume the server session is UTC (Dolt's
/// default), matching the Go reaper's `time.Now().UTC()` / `NOW()` convention.
///
/// Promotion enrichment (comments / references / `gt:keep`) is **not** populated by this
/// adapter: those live in side tables (`wisp_comments`, `wisp_dependencies`, `wisp_labels`)
/// that may sit on a different Dolt instance. The reaper therefore reaps Dolt wisps by age
/// alone — identical to the Go reaper — while the in-memory adapter exercises the
/// preserve-vs-reap promotion path in domain tests.
pub struct DoltWisp {
    pool: Pool,
}

impl DoltWisp {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    pub fn connect(url: &str) -> Result<Self, AppError> {
        Ok(Self::new(crate::conn::connect(url)?))
    }

    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    /// Create the `wisps` table if it doesn't exist. A no-op when the Go-written table is
    /// already present (its extra columns are untouched by this adapter's queries).
    pub async fn ensure_schema(&self) -> Result<(), AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        conn.query_drop(
            "CREATE TABLE IF NOT EXISTS wisps (
                id         VARCHAR(64)  PRIMARY KEY,
                wisp_type  VARCHAR(32)  NOT NULL,
                status     VARCHAR(16)  NOT NULL DEFAULT 'open',
                created_at DATETIME     NOT NULL,
                closed_at  DATETIME     NULL
            )",
        )
        .await
        .map_err(map_err)
    }

    /// Clear the table (tests / slate limpio).
    pub async fn truncate(&self) -> Result<(), AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        conn.query_drop("DELETE FROM wisps").await.map_err(map_err)
    }
}

/// Open-ish wire statuses are folded to [`WispStatus::Open`]; only an explicit `closed` is
/// terminal. Matches the Go reaper's `status IN ('open','hooked','in_progress')` predicate.
fn parse_status(s: &str) -> WispStatus {
    match s {
        "closed" => WispStatus::Closed,
        _ => WispStatus::Open,
    }
}

/// Build a `Wisp` from a row, or `None` if `wisp_type` is outside our TTL table (a foreign
/// kind we can't classify) or a timestamp is unrepresentable — such rows are skipped, not
/// reaped, so an unknown wisp is never compacted on the wrong policy.
fn row_to_wisp(
    (id, wisp_type, status, created_unix, closed_unix): (String, String, String, i64, Option<i64>),
) -> Option<Wisp> {
    let kind = WispKind::parse(&wisp_type).ok()?;
    let created_at = OffsetDateTime::from_unix_timestamp(created_unix).ok()?;
    let closed_at = match closed_unix {
        Some(secs) => Some(OffsetDateTime::from_unix_timestamp(secs).ok()?),
        None => None,
    };
    Some(Wisp {
        id,
        kind,
        status: parse_status(&status),
        created_at,
        closed_at,
        comment_count: 0,
        referenced: false,
        has_keep_label: false,
    })
}

const SELECT_COLS: &str =
    "SELECT id, wisp_type, status, UNIX_TIMESTAMP(created_at), UNIX_TIMESTAMP(closed_at) FROM wisps";

impl WispRepository for DoltWisp {
    async fn upsert(&self, wisp: &Wisp) -> Result<(), AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        conn.exec_drop(
            "REPLACE INTO wisps (id, wisp_type, status, created_at, closed_at)
             VALUES (:id, :wisp_type, :status, FROM_UNIXTIME(:created), FROM_UNIXTIME(:closed))",
            params! {
                "id" => &wisp.id,
                "wisp_type" => wisp.kind.as_str(),
                "status" => wisp.status.as_str(),
                "created" => wisp.created_at.unix_timestamp(),
                "closed" => wisp.closed_at.map(|c| c.unix_timestamp()),
            },
        )
        .await
        .map_err(map_err)
    }

    async fn get(&self, id: &str) -> Result<Option<Wisp>, AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        let row: Option<(String, String, String, i64, Option<i64>)> = conn
            .exec_first(
                format!("{SELECT_COLS} WHERE id = :id"),
                params! { "id" => id },
            )
            .await
            .map_err(map_err)?;
        Ok(row.and_then(row_to_wisp))
    }

    async fn list_open(&self) -> Result<Vec<Wisp>, AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        let rows: Vec<(String, String, String, i64, Option<i64>)> = conn
            .query(format!(
                "{SELECT_COLS} WHERE status IN ('open', 'hooked', 'in_progress') ORDER BY id"
            ))
            .await
            .map_err(map_err)?;
        Ok(rows.into_iter().filter_map(row_to_wisp).collect())
    }

    async fn list_closed_before(&self, cutoff: OffsetDateTime) -> Result<Vec<Wisp>, AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        let rows: Vec<(String, String, String, i64, Option<i64>)> = conn
            .exec(
                format!(
                    "{SELECT_COLS} WHERE status = 'closed' AND closed_at IS NOT NULL \
                     AND UNIX_TIMESTAMP(closed_at) < :cutoff ORDER BY id"
                ),
                params! { "cutoff" => cutoff.unix_timestamp() },
            )
            .await
            .map_err(map_err)?;
        Ok(rows.into_iter().filter_map(row_to_wisp).collect())
    }

    async fn mark_reaped(&self, id: &str, closed_at: OffsetDateTime) -> Result<bool, AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        conn.exec_drop(
            "UPDATE wisps SET status = 'closed', closed_at = FROM_UNIXTIME(:closed)
             WHERE id = :id AND status IN ('open', 'hooked', 'in_progress')",
            params! {
                "id" => id,
                "closed" => closed_at.unix_timestamp(),
            },
        )
        .await
        .map_err(map_err)?;
        Ok(conn.affected_rows() > 0)
    }

    async fn purge(&self, id: &str) -> Result<bool, AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        conn.exec_drop("DELETE FROM wisps WHERE id = :id", params! { "id" => id })
            .await
            .map_err(map_err)?;
        Ok(conn.affected_rows() > 0)
    }
}
