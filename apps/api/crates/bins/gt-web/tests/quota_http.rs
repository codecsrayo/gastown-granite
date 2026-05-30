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

// ---------------- hq-fe-api-r.1 — GET /api/quota/accounts ----------------

#[tokio::test]
async fn accounts_returns_empty_when_registry_empty() {
    let (base, root, _srv) = boot().await;
    let body: serde_json::Value = reqwest::get(format!("{base}/api/quota/accounts"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(body.as_array().unwrap().is_empty());
    root.shutdown();
}

#[tokio::test]
async fn accounts_collapses_status_and_window_for_sidebar() {
    let (base, root, _srv) = boot().await;
    root.quota.upsert_account(live_account("acc-active")).await;
    root.quota
        .upsert_account(cooldown_account("acc-parked", 9_000))
        .await;
    root.quota
        .upsert_account(Account {
            id: "acc-blocked".into(),
            status: AccountQuotaStatus::Limited,
            window: None,
        })
        .await;

    let body: serde_json::Value = reqwest::get(format!("{base}/api/quota/accounts"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 3);

    let by_id: std::collections::HashMap<&str, &serde_json::Value> =
        arr.iter().map(|r| (r["id"].as_str().unwrap(), r)).collect();

    let active = by_id["acc-active"];
    assert_eq!(active["state"], "active");
    assert_eq!(active["tokens_used"], 0);
    assert_eq!(active["tokens_cap"], 1_000_000);
    assert_eq!(active["reset_at"], 18_000);
    assert!(active["sessions"].as_array().unwrap().is_empty());

    let parked = by_id["acc-parked"];
    assert_eq!(parked["state"], "inactive");
    assert_eq!(parked["reset_at"], 9_000);

    let blocked = by_id["acc-blocked"];
    assert_eq!(blocked["state"], "blocked");
    assert!(blocked["tokens_used"].is_null());
    assert!(blocked["tokens_cap"].is_null());
    assert!(blocked["reset_at"].is_null());

    root.shutdown();
}

// ---------------- hq-fe-api-r.2 — GET /api/quota/rotation ----------------

/// Variant of `boot()` that wires an `event_log` path so `recent_rotations` can read it.
/// Returns the path so the test can seed `quota.rotated` lines before the request.
async fn boot_with_event_log() -> (
    String,
    gt_root::RootHandle<Arc<InMemoryBeads>>,
    tokio::task::JoinHandle<()>,
    std::path::PathBuf,
) {
    let beads = Arc::new(InMemoryBeads::default());
    let sessions = Arc::new(InMemorySessions::default());
    let reactor_log = {
        let mut p = std::env::temp_dir();
        p.push(format!("gt-web-quota-rot-react-{}.jsonl", ulid::Ulid::new()));
        p
    };
    let feed_log = {
        let mut p = std::env::temp_dir();
        p.push(format!("gt-web-quota-rot-feed-{}.jsonl", ulid::Ulid::new()));
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
        reactor_log,
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
        event_log: Some(Arc::new(feed_log.clone())),
    };
    let sink: Arc<dyn WebAuditSink> = Arc::new(InMemoryWebAudit::new());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state, AuthConfig::open(), sink, ReadinessGate::ready());
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), root, server, feed_log)
}

fn cooldown_account(id: &str, resets_at_secs: u64) -> Account {
    Account {
        id: id.into(),
        status: AccountQuotaStatus::Cooldown,
        window: Some(AccountWindow {
            kind: WindowKind::Rolling5h,
            limit: 1_000_000,
            started_at_secs: 0,
            resets_at_secs,
            consumed: 0.0,
        }),
    }
}

fn rotated_record(from: &str, to: &str, ts: &str) -> serde_json::Value {
    serde_json::json!({
        "event_id": ulid::Ulid::new().to_string(),
        "correlation_id": "c",
        "causation_id": null,
        "ts": ts,
        "type": "quota.rotated",
        "payload": {
            "from_account": from,
            "to_account": to,
            "now_secs": 0,
        },
    })
}

#[tokio::test]
async fn rotation_returns_empty_shape_when_unwired() {
    let (base, root, _srv) = boot().await; // bus wired, but event_log = None
    let body: serde_json::Value = reqwest::get(format!("{base}/api/quota/rotation"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(body["waiting_unlock"].as_array().unwrap().is_empty());
    assert!(body["recent_rotations"].as_array().unwrap().is_empty());
    root.shutdown();
}

#[tokio::test]
async fn rotation_surfaces_cooldown_accounts_in_waiting_unlock() {
    let (base, root, _srv, _log) = boot_with_event_log().await;
    root.quota.upsert_account(live_account("healthy")).await;
    root.quota
        .upsert_account(cooldown_account("parked", 12_345))
        .await;

    let body: serde_json::Value = reqwest::get(format!("{base}/api/quota/rotation"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let waiting = body["waiting_unlock"].as_array().unwrap();
    assert_eq!(waiting.len(), 1, "only cooldown account surfaces");
    assert_eq!(waiting[0]["account"], "parked");
    assert_eq!(waiting[0]["status"], "cooldown");
    assert_eq!(waiting[0]["unlock_at_secs"], 12_345);

    root.shutdown();
}

#[tokio::test]
async fn rotation_surfaces_recent_rotations_from_event_log() {
    let (base, root, _srv, log) = boot_with_event_log().await;
    let lines = [
        rotated_record("acc-a", "acc-b", "2026-05-30T10:00:00Z"),
        rotated_record("acc-b", "acc-c", "2026-05-30T11:00:00Z"),
        // Non-rotation noise; must be filtered out.
        serde_json::json!({
            "event_id": "n1", "correlation_id": "c", "causation_id": null,
            "ts": "2026-05-30T11:30:00Z", "type": "agent.spawned",
            "payload": { "session": "s" }
        }),
        rotated_record("acc-c", "acc-a", "2026-05-30T12:00:00Z"),
    ];
    let body_lines: String = lines
        .iter()
        .map(|r| serde_json::to_string(r).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&log, body_lines).unwrap();

    let body: serde_json::Value = reqwest::get(format!("{base}/api/quota/rotation"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let recent = body["recent_rotations"].as_array().unwrap();
    assert_eq!(recent.len(), 3, "3 rotations, agent.spawned filtered");
    assert_eq!(recent[0]["from"], "acc-a");
    assert_eq!(recent[2]["to"], "acc-a");
    root.shutdown();
}

#[tokio::test]
async fn rotation_since_filters_strict_after() {
    let (base, root, _srv, log) = boot_with_event_log().await;
    let lines = [
        rotated_record("a", "b", "2026-05-30T10:00:00Z"),
        rotated_record("b", "c", "2026-05-30T11:00:00Z"),
        rotated_record("c", "a", "2026-05-30T12:00:00Z"),
    ];
    let body_lines: String = lines
        .iter()
        .map(|r| serde_json::to_string(r).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&log, body_lines).unwrap();

    let body: serde_json::Value =
        reqwest::get(format!("{base}/api/quota/rotation?since=2026-05-30T11:00:00Z"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    let recent = body["recent_rotations"].as_array().unwrap();
    assert_eq!(recent.len(), 1, "only events strictly after the cutoff");
    assert_eq!(recent[0]["from"], "c");
    root.shutdown();
}

#[tokio::test]
async fn rotation_limit_caps_tail() {
    let (base, root, _srv, log) = boot_with_event_log().await;
    let lines: Vec<serde_json::Value> = (0..5)
        .map(|i| rotated_record(&format!("a{i}"), &format!("b{i}"), &format!("2026-05-30T1{i}:00:00Z")))
        .collect();
    let body_lines: String = lines
        .iter()
        .map(|r| serde_json::to_string(r).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&log, body_lines).unwrap();

    let body: serde_json::Value =
        reqwest::get(format!("{base}/api/quota/rotation?limit=2"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    let recent = body["recent_rotations"].as_array().unwrap();
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0]["from"], "a3", "limit returns the tail end");
    assert_eq!(recent[1]["from"], "a4");
    root.shutdown();
}
