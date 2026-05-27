//! Step 6.c gate: the **same** `QuotaRepository` contract runs against two impls:
//!   1. `InMemoryQuota` — always (host, no DB).
//!   2. `PgQuota` — against a real Postgres if `GT_PG_URL` is set. If one passes and the
//!      other fails, the port is mis-defined (same principle as the Step 4 Dolt gate).

use gt_quota::{
    Account, AccountQuotaStatus, AccountWindow, InMemoryQuota, QuotaRepository, UsageSample,
    WindowKind,
};
use gt_store_pg::PgQuota;

const W_START: u64 = 1_700_000_000;
const W_RESETS: u64 = W_START + 18_000;

async fn quota_repo_contract<R: QuotaRepository>(repo: &R) {
    // upsert + get accounts
    let acc = Account {
        id: "acc-x".to_string(),
        status: AccountQuotaStatus::Healthy,
        window: Some(AccountWindow {
            kind: WindowKind::Rolling5h,
            limit: 2_000,
            started_at_secs: W_START,
            resets_at_secs: W_RESETS,
            consumed: 0.0,
        }),
    };
    repo.upsert_account(&acc).await.unwrap();
    let got = repo.get_account("acc-x").await.unwrap().unwrap();
    assert_eq!(got.status, AccountQuotaStatus::Healthy);
    assert!(got.window.is_some());
    let w = got.window.unwrap();
    assert_eq!(w.limit, 2_000);
    assert_eq!(w.kind, WindowKind::Rolling5h);

    // status set
    repo.set_account_status("acc-x", AccountQuotaStatus::Limited)
        .await
        .unwrap();
    let got = repo.get_account("acc-x").await.unwrap().unwrap();
    assert_eq!(got.status, AccountQuotaStatus::Limited);
    // status for a non-existent account -> NotFound
    assert!(repo
        .set_account_status("nope", AccountQuotaStatus::Blocked)
        .await
        .is_err());

    // insert_sample append-only + per-account and per-session sums
    for minute in 0..3u64 {
        repo.insert_sample(&UsageSample {
            account: "acc-x".into(),
            session: "sess-a".into(),
            model: "opus".into(),
            ts_secs: W_START + minute * 60,
            input_tokens: 100,
            output_tokens: 100,
            cache_read: 0,
            cache_creation: 0,
        })
        .await
        .unwrap();
        repo.insert_sample(&UsageSample {
            account: "acc-x".into(),
            session: "sess-b".into(),
            model: "opus".into(),
            ts_secs: W_START + minute * 60,
            input_tokens: 50,
            output_tokens: 50,
            cache_read: 0,
            cache_creation: 0,
        })
        .await
        .unwrap();
    }

    let sess_a = repo
        .session_window_tokens("acc-x", "sess-a", W_START, W_RESETS)
        .await
        .unwrap();
    let sess_b = repo
        .session_window_tokens("acc-x", "sess-b", W_START, W_RESETS)
        .await
        .unwrap();
    let total = repo
        .account_window_tokens("acc-x", W_START, W_RESETS)
        .await
        .unwrap();
    assert_eq!(sess_a, 600);
    assert_eq!(sess_b, 300);
    assert_eq!(total, 900);

    // Range outside the window -> 0
    let outside = repo
        .account_window_tokens("acc-x", W_RESETS, W_RESETS + 60)
        .await
        .unwrap();
    assert_eq!(outside, 0);

    // Non-existent session -> 0
    let none = repo
        .session_window_tokens("acc-x", "nope", W_START, W_RESETS)
        .await
        .unwrap();
    assert_eq!(none, 0);
}

#[tokio::test]
async fn contract_in_memory() {
    let repo = InMemoryQuota::default();
    quota_repo_contract(&repo).await;
}

#[tokio::test]
async fn contract_postgres() {
    // GT_PG_URL = full URL to the server with the test DB created up front, e.g.
    // "postgres://gt:gt@127.0.0.1:5432/gt_rs_test". If unset, skip (host without Postgres).
    let Ok(url) = std::env::var("GT_PG_URL") else {
        eprintln!("GT_PG_URL unset — skipping the Postgres contract (run with Postgres available)");
        return;
    };
    let repo = PgQuota::connect(&url).await.expect("connect Postgres");
    gt_store_pg::ensure_schema(repo.pool())
        .await
        .expect("ensure_schema");
    repo.truncate().await.expect("clean tables");
    quota_repo_contract(&repo).await;
}
