//! Gate for hq-fe-api-w.11: `POST /api/beads/bulk` + per-actor rate-limit middleware.
//!
//! Bulk handler validates every item against the same rules as `POST /api/beads`,
//! rejects the whole batch on the first failure (no partial success), and caps the
//! per-request item count at `BULK_BEADS_MAX_ITEMS`. The rate-limit middleware fronts
//! the route so a runaway script cannot exhaust the dispatcher's per-actor budget.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::net::TcpListener;

use gt_agent::InMemorySessions;
use gt_beads::InMemoryBeads;
use gt_root::{root::Effects, spawn, RootConfig, SystemClock};
use gt_web::{
    router_with_stores, AppState, AuthConfig, IdempotencyStore, InMemoryWebAudit,
    RateLimitStore, ReadinessGate, WebAuditSink,
};

struct NoopEffects;
impl Effects for NoopEffects {
    fn sling(&self, _convoy: &str, _member: &str) {}
    fn rotate(&self, _account: &str) {}
}

async fn boot(rate_limit: RateLimitStore) -> (String, gt_root::RootHandle<Arc<InMemoryBeads>>, tokio::task::JoinHandle<()>) {
    let beads = Arc::new(InMemoryBeads::default());
    let sessions = Arc::new(InMemorySessions::default());
    let log = {
        let mut p = std::env::temp_dir();
        p.push(format!("gt-web-bulk-{}.jsonl", ulid::Ulid::new()));
        p
    };
    let merges = Arc::new(gt_merge::InMemoryMergeRepo::default());
    let root = spawn(
        beads.clone(),
        merges.clone(),
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
        merges: merges.clone(),
        agent_events: root.agent_events.clone(),
        events: root.events_sender(),
        town_root: None,
        issues: None,
        bus: Some(root.commands()),
        worktrees_stream: None,
        control: None,
        respawner: None,
        commenter: None,
        event_log: None,
        login_registry: std::sync::Arc::new(gt_web::LoginRegistry::new()),
        login_pty: None,
        login_config: std::sync::Arc::new(gt_web::LoginConfig::default()),
         terminal_attach: None,
    };
    let sink: Arc<dyn WebAuditSink> = Arc::new(InMemoryWebAudit::new());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router_with_stores(
        state,
        AuthConfig::open(),
        sink,
        ReadinessGate::ready(),
        IdempotencyStore::with_defaults(),
        rate_limit,
    );
    let srv = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), root, srv)
}

#[tokio::test]
async fn bulk_creates_all_items_and_returns_201() {
    let (base, root, _srv) = boot(RateLimitStore::new(Duration::from_secs(60), 100)).await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/beads/bulk"))
        .json(&json!({ "beads": [
            { "id": "hq-bk-1", "title": "first", "priority": 1 },
            { "id": "hq-bk-2", "title": "second", "priority": 2, "assignee": "alice" },
        ] }))
        .send()
        .await
        .expect("send post");
    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.expect("json");
    let created = body["created"].as_array().expect("array");
    assert_eq!(created.len(), 2);
    assert_eq!(created[0]["id"], "hq-bk-1");
    assert_eq!(created[0]["status"], "pending");
    assert_eq!(created[1]["assignee"], "alice");
    root.shutdown();
}

#[tokio::test]
async fn bulk_rejects_empty_array() {
    let (base, root, _srv) = boot(RateLimitStore::new(Duration::from_secs(60), 100)).await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/beads/bulk"))
        .json(&json!({ "beads": [] }))
        .send()
        .await
        .expect("send post");
    assert_eq!(resp.status(), 400);
    root.shutdown();
}

#[tokio::test]
async fn bulk_rejects_over_cap() {
    let (base, root, _srv) = boot(RateLimitStore::new(Duration::from_secs(60), 100)).await;
    let beads: Vec<serde_json::Value> = (0..101)
        .map(|i| json!({ "id": format!("hq-cap-{i}"), "title": "t", "priority": 0 }))
        .collect();
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/beads/bulk"))
        .json(&json!({ "beads": beads }))
        .send()
        .await
        .expect("send post");
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert!(
        body["error"].as_str().unwrap_or_default().contains("cap"),
        "error mentions cap: {body}",
    );
    root.shutdown();
}

#[tokio::test]
async fn bulk_rejects_invalid_item_and_does_not_persist_others() {
    // First item is fine, second has priority 9 — whole batch must fail without
    // creating the first row, since the bead spec requires atomic success.
    let (base, root, _srv) = boot(RateLimitStore::new(Duration::from_secs(60), 100)).await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/beads/bulk"))
        .json(&json!({ "beads": [
            { "id": "hq-bad-1", "title": "ok", "priority": 0 },
            { "id": "hq-bad-2", "title": "x", "priority": 9 },
        ] }))
        .send()
        .await
        .expect("send post");
    assert_eq!(resp.status(), 400);

    // List beads after — first row must NOT have been created.
    let list: serde_json::Value = reqwest::Client::new()
        .get(format!("{base}/api/beads?status=pending"))
        .send()
        .await
        .expect("send list")
        .json()
        .await
        .expect("json");
    let ids: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert!(!ids.contains(&"hq-bad-1"), "first row leaked: {ids:?}");
    root.shutdown();
}

#[tokio::test]
async fn bulk_rejects_duplicate_id_in_batch() {
    let (base, root, _srv) = boot(RateLimitStore::new(Duration::from_secs(60), 100)).await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/beads/bulk"))
        .json(&json!({ "beads": [
            { "id": "hq-dup-1", "title": "a", "priority": 0 },
            { "id": "hq-dup-1", "title": "b", "priority": 0 },
        ] }))
        .send()
        .await
        .expect("send post");
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert!(
        body["error"].as_str().unwrap_or_default().contains("duplicate"),
        "error mentions duplicate: {body}",
    );
    root.shutdown();
}

#[tokio::test]
async fn rate_limit_returns_429_after_cap() {
    // Cap = 2 per minute; the third bulk call must surface 429 + Retry-After.
    let (base, root, _srv) = boot(RateLimitStore::new(Duration::from_secs(60), 2)).await;
    let body = json!({ "beads": [
        { "id": format!("hq-rl-{}", ulid::Ulid::new()), "title": "t", "priority": 0 }
    ] });

    let client = reqwest::Client::new();
    for _ in 0..2 {
        let resp = client
            .post(format!("{base}/api/beads/bulk"))
            .json(&body)
            .send()
            .await
            .expect("send post");
        assert_eq!(resp.status(), 201);
        assert!(
            resp.headers().contains_key("x-ratelimit-remaining"),
            "success response carries x-ratelimit-remaining header"
        );
    }
    let limited = client
        .post(format!("{base}/api/beads/bulk"))
        .json(&body)
        .send()
        .await
        .expect("send post");
    assert_eq!(limited.status(), 429);
    let retry = limited.headers().get("retry-after").expect("retry-after").to_str().unwrap();
    assert!(retry.parse::<u64>().is_ok(), "retry-after is numeric: {retry}");
    root.shutdown();
}

#[tokio::test]
async fn rate_limit_does_not_throttle_non_bulk_routes() {
    // Even with a 1-per-minute bulk cap, the single-row POST /api/beads route is on a
    // separate budget. Confirms the rate-limit layer is scoped to /api/beads/bulk only.
    let (base, root, _srv) = boot(RateLimitStore::new(Duration::from_secs(60), 1)).await;
    let client = reqwest::Client::new();
    // First bulk call consumes the only slot.
    let bulk = client
        .post(format!("{base}/api/beads/bulk"))
        .json(&json!({ "beads": [
            { "id": "hq-iso-1", "title": "t", "priority": 0 }
        ] }))
        .send()
        .await
        .expect("send bulk");
    assert_eq!(bulk.status(), 201);

    // Single-row POST after still goes through — different route, no rate-limit.
    let single = client
        .post(format!("{base}/api/beads"))
        .json(&json!({ "id": "hq-iso-2", "title": "t", "priority": 0 }))
        .send()
        .await
        .expect("send single");
    assert_eq!(single.status(), 201);
    root.shutdown();
}
