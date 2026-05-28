use sqlx::PgPool;
use sqlx::Row;
use time::OffsetDateTime;

use gt_events::AppError;
use gt_quota::{Account, AccountQuotaStatus, AccountWindow, QuotaRepository, UsageSample};

use crate::conn::map_err;

/// Postgres adapter for the `QuotaRepository` port.
pub struct PgQuota {
    pool: PgPool,
}

impl PgQuota {
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
        sqlx::query("TRUNCATE TABLE token_usage RESTART IDENTITY")
            .execute(&self.pool)
            .await
            .map_err(map_err)?;
        sqlx::query("TRUNCATE TABLE accounts")
            .execute(&self.pool)
            .await
            .map_err(map_err)?;
        Ok(())
    }
}

fn ts_from_secs(secs: u64) -> Result<OffsetDateTime, AppError> {
    OffsetDateTime::from_unix_timestamp(secs as i64)
        .map_err(|e| AppError::Other(format!("ts_from_secs({secs}): {e}")))
}

fn status_as_str(s: AccountQuotaStatus) -> &'static str {
    match s {
        AccountQuotaStatus::Healthy => "healthy",
        AccountQuotaStatus::Limited => "limited",
        AccountQuotaStatus::Blocked => "blocked",
        AccountQuotaStatus::Cooldown => "cooldown",
    }
}

fn status_from_str(s: &str) -> AccountQuotaStatus {
    match s {
        "limited" => AccountQuotaStatus::Limited,
        "blocked" => AccountQuotaStatus::Blocked,
        "cooldown" => AccountQuotaStatus::Cooldown,
        _ => AccountQuotaStatus::Healthy,
    }
}

/// `AccountWindow` derives `Serialize`/`Deserialize`: the round-trip to the `JSONB` column
/// goes through `serde_json`, not an ad-hoc parser.
fn encode_window(w: &AccountWindow) -> Result<String, AppError> {
    serde_json::to_string(w).map_err(|e| AppError::Other(format!("window encode: {e}")))
}

fn decode_window(raw: Option<String>) -> Option<AccountWindow> {
    serde_json::from_str(&raw?).ok()
}

impl QuotaRepository for PgQuota {
    async fn insert_sample(&self, sample: &UsageSample) -> Result<(), AppError> {
        let ts = ts_from_secs(sample.ts_secs)?;
        sqlx::query(
            "INSERT INTO token_usage
               (account_id, session_id, model, ts, input_tokens, output_tokens, cache_read, cache_creation)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(&sample.account)
        .bind(&sample.session)
        .bind(&sample.model)
        .bind(ts)
        .bind(sample.input_tokens as i64)
        .bind(sample.output_tokens as i64)
        .bind(sample.cache_read as i64)
        .bind(sample.cache_creation as i64)
        .execute(&self.pool)
        .await
        .map_err(map_err)?;
        Ok(())
    }

    async fn account_window_tokens(
        &self,
        account: &str,
        from_secs: u64,
        to_secs: u64,
    ) -> Result<u64, AppError> {
        let from = ts_from_secs(from_secs)?;
        let to = ts_from_secs(to_secs)?;
        let row = sqlx::query(
            "SELECT COALESCE(SUM(input_tokens + output_tokens), 0)::BIGINT AS total
             FROM token_usage
             WHERE account_id = $1 AND ts >= $2 AND ts < $3",
        )
        .bind(account)
        .bind(from)
        .bind(to)
        .fetch_one(&self.pool)
        .await
        .map_err(map_err)?;
        let total: i64 = row.try_get("total").map_err(map_err)?;
        Ok(total.max(0) as u64)
    }

    async fn session_window_tokens(
        &self,
        account: &str,
        session: &str,
        from_secs: u64,
        to_secs: u64,
    ) -> Result<u64, AppError> {
        let from = ts_from_secs(from_secs)?;
        let to = ts_from_secs(to_secs)?;
        let row = sqlx::query(
            "SELECT COALESCE(SUM(input_tokens + output_tokens), 0)::BIGINT AS total
             FROM token_usage
             WHERE account_id = $1 AND session_id = $2 AND ts >= $3 AND ts < $4",
        )
        .bind(account)
        .bind(session)
        .bind(from)
        .bind(to)
        .fetch_one(&self.pool)
        .await
        .map_err(map_err)?;
        let total: i64 = row.try_get("total").map_err(map_err)?;
        Ok(total.max(0) as u64)
    }

    async fn upsert_account(&self, account: &Account) -> Result<(), AppError> {
        let window_json = match account.window.as_ref() {
            Some(w) => Some(encode_window(w)?),
            None => None,
        };
        sqlx::query(
            "INSERT INTO accounts (id, status, window_json)
             VALUES ($1, $2, $3::jsonb)
             ON CONFLICT (id) DO UPDATE
               SET status = EXCLUDED.status, window_json = EXCLUDED.window_json",
        )
        .bind(&account.id)
        .bind(status_as_str(account.status))
        .bind(window_json)
        .execute(&self.pool)
        .await
        .map_err(map_err)?;
        Ok(())
    }

    async fn get_account(&self, id: &str) -> Result<Option<Account>, AppError> {
        let row =
            sqlx::query("SELECT status, window_json::text AS window_json FROM accounts WHERE id = $1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .map_err(map_err)?;
        Ok(row.map(|r| {
            let status: String = r.try_get("status").unwrap_or_else(|_| "healthy".into());
            let window_raw: Option<String> = r.try_get("window_json").ok();
            Account {
                id: id.to_string(),
                status: status_from_str(&status),
                window: decode_window(window_raw),
            }
        }))
    }

    async fn set_account_status(
        &self,
        id: &str,
        status: AccountQuotaStatus,
    ) -> Result<(), AppError> {
        let res = sqlx::query("UPDATE accounts SET status = $1 WHERE id = $2")
            .bind(status_as_str(status))
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(map_err)?;
        if res.rows_affected() == 0 {
            return Err(AppError::NotFound(format!("account {id}")));
        }
        Ok(())
    }
}
