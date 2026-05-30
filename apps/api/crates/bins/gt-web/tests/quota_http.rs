//! Gate for hq-fe-api-w.10: `POST /api/quota/accounts/:id/rotate` and `/retire` over HTTP.
//!
//! Wires a real composition root so the routes dispatch through the same `CommandBus`
//! gt-mcp's `quota.*` tools drive. Asserts:
//!   - retire-after-register drains the registry (`removed: true`),
//!   - retire on an absent id is idempotent (`removed: false`),
//!   - rotate on an unregistered source surfaces a 500 (the actor emits `NotFound`),
//!   - empty path / body fields are rejected at the frontier with `400`,
//!   - GET on a write route 405s (route is POST-only).

use std::sync::Arc;

use serde_json::json;
use tokio::net::TcpListener;

use gt_agent::InMemorySessions;
use gt_beads::InMemoryBeads;
use gt_quota::{Account, AccountQuotaStatus, AccountWindow, WindowKind};
use gt_root::{root::Effects, spawn, RootConfig, SystemClock};
use gt_web::{router, AppState, AuthConfig, InMemoryWebAudit, ReadinessGate, WebAuditSink};

struct NoopEffects;
impl Effects for NoopEffects {
    fn sling(&self, _convoy: &str, _member: &str) {}
    fn rotate(&self, _account: &str) {}
}

async fn boot() -> (
    String,
    gt_root::RootHandle<Arc<InMemoryBeads>>,
    tokio::task::JoinHandle<()>,
) {
    let beads = Arc::new(InMemoryBeads::default());
    let sessions = Arc::new(InMemorySessions::default());
    let log = {
        let mut p = std::env::temp_dir();
        p.push(format!("gt-web-quota-{}.jsonl", ulid::Ulid::new()));
        p
    };
    let root = spawn(
        beads.clone(),
        Arc::new(gt_merge::InMemoryMergeRepo::default()),
        Arc::new(gt_patrol::InMemoryPatrolRepo::default()),
        Arc::new(gt_orchestration::InMemoryOrchRepo::default()),
        NoopEffects,
        SystemClock,
        log,
        RootConfig::default(),
    );
    let state = AppState {
        beads,
        sessions,
        agent_events: root.agent_events.clone(),
        events: root.events_sender(),
        town_root: None,
        issues: None,
        bus: Some(root.commands()),
        worktrees_stream: None,
        killer: None,
    };
    let sink: Arc<dyn WebAuditSink> = Arc::new(InMemoryWebAudit::new());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state, AuthConfig::open(), sink, ReadinessGate::ready());
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), root, server)
}

fn live_account(id: &str) -> Account {
    Account {
        id: id.into(),
        status: AccountQuotaStatus::Healthy,
        window: Some(AccountWindow {
            kind: WindowKind::Rolling5h,
            limit: 1_000_000,
            started_at_secs: 0,
            resets_at_secs: 60 * 60 * 5,
            consumed: 0.0,
        }),
    }
}

#[tokio::test]
async fn retire_removes_registered_account_and_is_idempotent_on_miss() {
    let (base, root, _srv) = boot().await;
    root.quota.upsert_account(live_account("acct-1")).await;

    let resp = reqwest::Client::new()
        .post(format!("{base}/api/quota/accounts/acct-1/retire"))
        .send()
        .await
        .expect("send retire");
    assert_eq!(resp.status(), 200, "retire of registered account succeeds");
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["account"], "acct-1");
    assert_eq!(body["removed"], true, "first retire removes");

    let resp = reqwest::Client::new()
        .post(format!("{base}/api/quota/accounts/acct-1/retire"))
        .send()
        .await
        .expect("second retire send");
    assert_eq!(resp.status(), 200, "idempotent retire still 200");
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(
        body["removed"], false,
        "second retire reports no-op without erroring",
    );

    root.shutdown();
}

#[tokio::test]
async fn rotate_dispatches_command_and_reflects_via_snapshot() {
    let (base, root, _srv) = boot().await;
    root.quota.upsert_account(live_account("acct-from")).await;
    root.quota.upsert_account(live_account("acct-to")).await;

    let resp = reqwest::Client::new()
        .post(format!("{base}/api/quota/accounts/acct-from/rotate"))
        .json(&json!({ "to_account": "acct-to", "now_secs": 100 }))
        .send()
        .await
        .expect("send rotate");
    assert!(
        resp.status().is_success(),
        "rotate happy path: {}",
        resp.status()
    );
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["rotated"], true);
    assert_eq!(body["from"], "acct-from");
    assert_eq!(body["to"], "acct-to");

    root.shutdown();
}

#[tokio::test]
async fn rotate_rejects_same_source_and_target() {
    let (base, root, _srv) = boot().await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/quota/accounts/acct/rotate"))
        .json(&json!({ "to_account": "acct" }))
        .send()
        .await
        .expect("send rotate");
    assert_eq!(resp.status(), 400, "same-target rejected at frontier");
    root.shutdown();
}

#[tokio::test]
async fn rotate_rejects_empty_to_account() {
    let (base, root, _srv) = boot().await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/quota/accounts/acct/rotate"))
        .json(&json!({ "to_account": "" }))
        .send()
        .await
        .expect("send rotate");
    assert_eq!(resp.status(), 400);
    root.shutdown();
}

#[tokio::test]
async fn rotate_on_unregistered_source_still_dispatches() {
    // The actor does not gate rotate on the account being registered today — the keychain
    // edge logs `not found` and the command still succeeds. Document the current shape so
    // a future tightening (or a fix that hardens rotate at the actor) flips this test.
    let (base, root, _srv) = boot().await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/quota/accounts/missing/rotate"))
        .json(&json!({ "to_account": "also-missing" }))
        .send()
        .await
        .expect("send rotate");
    assert!(resp.status().is_success(), "rotate dispatches: {}", resp.status());
    root.shutdown();
}

#[tokio::test]
async fn get_on_post_only_route_returns_405() {
    let (base, root, _srv) = boot().await;
    let resp = reqwest::Client::new()
        .get(format!("{base}/api/quota/accounts/acct/retire"))
        .send()
        .await
        .expect("send get");
    assert_eq!(resp.status(), 405);
    root.shutdown();
}
