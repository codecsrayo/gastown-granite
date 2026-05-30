//! Gate for hq-fe-auth.2: `POST /api/quota/accounts/:id/login{,/token,/cancel}`.
//!
//! Wires a real composition root + `gt_login::FakePty` so the driver runs end-to-end
//! against scripted output without touching a real `claude` binary. Asserts:
//!
//! - start returns 202 + `{flow_id, account}`,
//! - a second start while a flow is in flight returns 409 with the live `flow_id`,
//! - token submission with a matching `flow_id` wakes the driver, the script's exit
//!   status decides success/failure, and the registry slot is cleared,
//! - cancel returns 200 + the live `flow_id` and short-circuits the driver to
//!   `quota.login_failed{Cancelled}`,
//! - `quota.login_*` events surface on the running root's event broadcast in the
//!   exact order the bead spec lists (`started → url_ready → complete | failed`).

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::broadcast::error::TryRecvError;

use gt_agent::InMemorySessions;
use gt_audit::EventRecord;
use gt_beads::InMemoryBeads;
use gt_login::FakePty;
use gt_root::{root::Effects, spawn, RootConfig, SystemClock};
use gt_web::{
    router, AppState, AuthConfig, InMemoryWebAudit, LoginConfig, LoginRegistry, ReadinessGate,
    WebAuditSink,
};

struct NoopEffects;
impl Effects for NoopEffects {
    fn sling(&self, _convoy: &str, _member: &str) {}
    fn rotate(&self, _account: &str) {}
}

async fn boot_with_pty(
    pty: Arc<dyn gt_login::Pty>,
) -> (
    String,
    gt_root::RootHandle<Arc<InMemoryBeads>>,
    tokio::sync::broadcast::Receiver<EventRecord>,
    tokio::task::JoinHandle<()>,
) {
    let beads = Arc::new(InMemoryBeads::default());
    let sessions = Arc::new(InMemorySessions::default());
    let log = {
        let mut p = std::env::temp_dir();
        p.push(format!("gt-web-login-{}.jsonl", ulid::Ulid::new()));
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
    // Subscribe BEFORE the driver task starts so no `quota.login_*` frames are lost.
    let events_rx = root.subscribe_events();
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
        login_registry: Arc::new(LoginRegistry::new()),
        login_pty: Some(pty),
        login_config: Arc::new(LoginConfig::default()),
    };
    let sink: Arc<dyn WebAuditSink> = Arc::new(InMemoryWebAudit::new());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state, AuthConfig::open(), sink, ReadinessGate::ready());
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), root, events_rx, server)
}

/// Read every `quota.login_*` frame currently queued on the broadcast receiver.
/// Bounded by a short timeout so a test failure surfaces as a missing frame, not a hang.
async fn drain_login_events(
    rx: &mut tokio::sync::broadcast::Receiver<EventRecord>,
    expected: usize,
) -> Vec<EventRecord> {
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while out.len() < expected && tokio::time::Instant::now() < deadline {
        match rx.try_recv() {
            Ok(rec) if rec.kind.starts_with("quota.login_") => out.push(rec),
            Ok(_) => continue,
            Err(TryRecvError::Empty) => tokio::time::sleep(Duration::from_millis(10)).await,
            Err(TryRecvError::Closed) => break,
            Err(TryRecvError::Lagged(_)) => continue,
        }
    }
    out
}

#[tokio::test]
async fn start_returns_202_with_flow_id_and_emits_started_then_url_ready() {
    // Script that only prints the URL — driver blocks in phase 2 waiting on the token,
    // so the flow stays in flight (registry slot remains populated) until we cancel.
    let pty = Arc::new(FakePty::scripted(
        vec![b"Open https://console.anthropic.com/oauth?state=xyz\n".to_vec()],
        0,
    )) as Arc<dyn gt_login::Pty>;
    let (base, root, mut events, _srv) = boot_with_pty(pty).await;

    let resp = reqwest::Client::new()
        .post(format!("{base}/api/quota/accounts/acct-1/login"))
        .send()
        .await
        .expect("send start");
    assert_eq!(resp.status(), 202);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["account"], "acct-1");
    let flow_id = body["flow_id"].as_str().unwrap().to_string();
    assert!(!flow_id.is_empty(), "flow_id is minted");

    let frames = drain_login_events(&mut events, 2).await;
    let kinds: Vec<_> = frames.iter().map(|r| r.kind.as_str()).collect();
    assert_eq!(kinds, vec!["quota.login_started", "quota.login_url_ready"]);
    assert_eq!(frames[1].payload["url"], "https://console.anthropic.com/oauth?state=xyz");
    assert_eq!(frames[0].payload["flow_id"], flow_id);

    // Clean up the in-flight slot so the spawn_blocking task does not outlive the test.
    let _ = reqwest::Client::new()
        .post(format!("{base}/api/quota/accounts/acct-1/login/cancel"))
        .send()
        .await;
    root.shutdown();
}

#[tokio::test]
async fn second_start_while_in_flight_409s_with_live_flow_id() {
    let pty = Arc::new(FakePty::scripted(
        vec![b"https://console.anthropic.com/oauth?x=1\n".to_vec()],
        0,
    )) as Arc<dyn gt_login::Pty>;
    let (base, root, _events, _srv) = boot_with_pty(pty).await;

    let first = reqwest::Client::new()
        .post(format!("{base}/api/quota/accounts/acct-1/login"))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 202);
    let body: serde_json::Value = first.json().await.unwrap();
    let live = body["flow_id"].as_str().unwrap().to_string();

    let second = reqwest::Client::new()
        .post(format!("{base}/api/quota/accounts/acct-1/login"))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 409, "second start collides");
    let body: serde_json::Value = second.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains(&live),
        "409 message echoes live flow_id so the UI can re-attach: {body}",
    );

    let _ = reqwest::Client::new()
        .post(format!("{base}/api/quota/accounts/acct-1/login/cancel"))
        .send()
        .await;
    root.shutdown();
}

#[tokio::test]
async fn token_submission_drives_driver_to_complete_and_clears_slot() {
    let pty = Arc::new(FakePty::scripted(
        vec![b"https://console.anthropic.com/x?y=1\n".to_vec()],
        0, // exit 0 → Complete
    )) as Arc<dyn gt_login::Pty>;
    let (base, root, mut events, _srv) = boot_with_pty(pty).await;

    let start: serde_json::Value = reqwest::Client::new()
        .post(format!("{base}/api/quota/accounts/acct-1/login"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let flow_id = start["flow_id"].as_str().unwrap().to_string();

    // Submit token. Driver wakes, writes to PTY, waits for exit (script returns 0 →
    // Complete).
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/quota/accounts/acct-1/login/token"))
        .json(&json!({ "flow_id": flow_id, "token": "TOK-123" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let frames = drain_login_events(&mut events, 3).await;
    let kinds: Vec<_> = frames.iter().map(|r| r.kind.as_str()).collect();
    assert_eq!(
        kinds,
        vec![
            "quota.login_started",
            "quota.login_url_ready",
            "quota.login_complete",
        ]
    );
    assert_eq!(frames[2].payload["account"], "acct-1");

    // Second cancel after Complete must 404 — slot was cleared by the driver thread.
    let post = reqwest::Client::new()
        .post(format!("{base}/api/quota/accounts/acct-1/login/cancel"))
        .send()
        .await
        .unwrap();
    assert_eq!(post.status(), 404);
    root.shutdown();
}

#[tokio::test]
async fn cancel_aborts_flow_and_emits_failed_cancelled() {
    let pty = Arc::new(FakePty::scripted(
        vec![b"https://console.anthropic.com/x\n".to_vec()],
        0,
    )) as Arc<dyn gt_login::Pty>;
    let (base, root, mut events, _srv) = boot_with_pty(pty).await;

    let start: serde_json::Value = reqwest::Client::new()
        .post(format!("{base}/api/quota/accounts/acct-1/login"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let flow_id = start["flow_id"].as_str().unwrap().to_string();

    let resp = reqwest::Client::new()
        .post(format!("{base}/api/quota/accounts/acct-1/login/cancel"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["flow_id"], flow_id);

    let frames = drain_login_events(&mut events, 3).await;
    let kinds: Vec<_> = frames.iter().map(|r| r.kind.as_str()).collect();
    assert_eq!(
        kinds,
        vec![
            "quota.login_started",
            "quota.login_url_ready",
            "quota.login_failed",
        ]
    );
    // Reason is the typed `LoginFailure::Cancelled` variant — flat string fallback in
    // `message` lets the UI render without typing the enum upfront.
    assert_eq!(frames[2].payload["reason"]["kind"], "cancelled");
    assert!(frames[2].payload["message"]
        .as_str()
        .unwrap()
        .contains("cancelled"));
    root.shutdown();
}

#[tokio::test]
async fn token_with_wrong_flow_id_returns_404() {
    let pty = Arc::new(FakePty::scripted(
        vec![b"https://console.anthropic.com/x\n".to_vec()],
        0,
    )) as Arc<dyn gt_login::Pty>;
    let (base, root, _events, _srv) = boot_with_pty(pty).await;

    let _start: serde_json::Value = reqwest::Client::new()
        .post(format!("{base}/api/quota/accounts/acct-1/login"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let resp = reqwest::Client::new()
        .post(format!("{base}/api/quota/accounts/acct-1/login/token"))
        .json(&json!({ "flow_id": "WRONG", "token": "TOK" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    let _ = reqwest::Client::new()
        .post(format!("{base}/api/quota/accounts/acct-1/login/cancel"))
        .send()
        .await;
    root.shutdown();
}

#[tokio::test]
async fn start_without_pty_returns_503() {
    let beads = Arc::new(InMemoryBeads::default());
    let sessions = Arc::new(InMemorySessions::default());
    let log = {
        let mut p = std::env::temp_dir();
        p.push(format!("gt-web-login-nopty-{}.jsonl", ulid::Ulid::new()));
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
        login_registry: Arc::new(LoginRegistry::new()),
        // No PTY wired — start must 503, not panic.
        login_pty: None,
        login_config: Arc::new(LoginConfig::default()),
    };
    let sink: Arc<dyn WebAuditSink> = Arc::new(InMemoryWebAudit::new());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state, AuthConfig::open(), sink, ReadinessGate::ready());
    let _srv = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/api/quota/accounts/acct-1/login"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
    root.shutdown();
}
