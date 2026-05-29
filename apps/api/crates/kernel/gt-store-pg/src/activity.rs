//! Read-side of the activity projection (hq-mysw). SQL twin of `gt_feed::activity_view`.
//!
//! The table stores only `last_activity_secs` (clock-free, set by the drain from each event's
//! own `ts`). The color is computed at read time against an injected `now_secs`, reusing
//! `gt_feed::activity::ActivityInfo` so the SQL read-side and the in-memory view share one set
//! of thresholds. Panels that want pure SQL can color-code with a `CASE` on
//! `(now - last_activity_secs)` directly against `activity_projections`.

use sqlx::{PgPool, Row};

use gt_events::AppError;
use gt_feed::activity::ActivityInfo;

use crate::conn::map_err;

/// One projected correlation lifeline with its color code as of the supplied `now`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityEntry {
    pub subject: String,
    pub last_kind: String,
    pub last_activity_secs: i64,
    pub activity: ActivityInfo,
}

/// Read-side over `activity_projections`.
pub struct PgActivity {
    pool: PgPool,
}

impl PgActivity {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn connect(url: &str) -> Result<Self, AppError> {
        Ok(Self::new(crate::conn::connect(url).await?))
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Color-coded activity for one subject (correlation id) as of `now_secs`, or `None` if the
    /// subject has no recorded activity. Age is clamped at 0 for clock skew (same as the view).
    pub async fn status(&self, subject: &str, now_secs: u64) -> Result<Option<ActivityInfo>, AppError> {
        let row = sqlx::query(
            "SELECT last_activity_secs FROM activity_projections WHERE subject = $1",
        )
        .bind(subject)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_err)?;
        Ok(row.map(|r| {
            let secs: i64 = r.try_get("last_activity_secs").unwrap_or(0);
            ActivityInfo::from_age(now_secs.saturating_sub(secs.max(0) as u64))
        }))
    }

    /// All subjects, most-recently-active first, color-coded as of `now_secs`.
    pub async fn all(&self, now_secs: u64) -> Result<Vec<ActivityEntry>, AppError> {
        let rows = sqlx::query(
            "SELECT subject, last_kind, last_activity_secs
             FROM activity_projections
             ORDER BY last_activity_secs DESC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(map_err)?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let subject: String = r.try_get("subject").unwrap_or_default();
                let last_kind: String = r.try_get("last_kind").unwrap_or_default();
                let last_activity_secs: i64 = r.try_get("last_activity_secs").unwrap_or(0);
                ActivityEntry {
                    subject,
                    last_kind,
                    last_activity_secs,
                    activity: ActivityInfo::from_age(
                        now_secs.saturating_sub(last_activity_secs.max(0) as u64),
                    ),
                }
            })
            .collect())
    }

    pub async fn truncate(&self) -> Result<(), AppError> {
        sqlx::query("TRUNCATE TABLE activity_projections")
            .execute(&self.pool)
            .await
            .map_err(map_err)?;
        Ok(())
    }
}
