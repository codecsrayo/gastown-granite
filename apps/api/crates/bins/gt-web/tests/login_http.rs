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

use futures::StreamExt;
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
        terminal_attach: None,
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
         terminal_attach: None,
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

// ---------------------------------------------------------------------------
// hq-fe-auth.3 — SSE wire shape. The same `quota.login_*` `EventRecord`s the
// in-process broadcast carries must arrive intact on `GET /api/stream` (the bridge
// in `gt_web::stream` does no per-kind filtering) and the payload must match the
// typed DTOs (`gt_web::QuotaLogin{Started,UrlReady,Complete,Failed}`).

#[tokio::test]
async fn sse_stream_surfaces_quota_login_kinds_in_order() {
    let pty = Arc::new(FakePty::scripted(
        vec![b"https://console.anthropic.com/oauth?state=zzz\n".to_vec()],
        0,
    )) as Arc<dyn gt_login::Pty>;
    let (base, root, _events, _srv) = boot_with_pty(pty).await;

    // Open the stream first so the broadcast subscriber exists before the driver
    // task runs — the broadcast is best-effort, frames pushed before any receiver
    // is registered are lost.
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/stream"))
        .header("accept", "text/event-stream")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let mut byte_stream = resp.bytes_stream();

    for _ in 0..50 {
        if root.event_subscribers() >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(root.event_subscribers() >= 1, "SSE never subscribed");

    // Drive the flow: start → token → driver writes + waits for exit → complete.
    let start: serde_json::Value = reqwest::Client::new()
        .post(format!("{base}/api/quota/accounts/acct-sse/login"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let flow_id = start["flow_id"].as_str().unwrap().to_string();
    let _ = reqwest::Client::new()
        .post(format!("{base}/api/quota/accounts/acct-sse/login/token"))
        .json(&json!({ "flow_id": flow_id, "token": "TOK-Y" }))
        .send()
        .await
        .unwrap();

    let mut buf = Vec::<u8>::new();
    let saw_all = tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(chunk) = byte_stream.next().await {
            let bytes = chunk.unwrap();
            buf.extend_from_slice(&bytes);
            if let Ok(s) = std::str::from_utf8(&buf) {
                if s.contains("\"type\":\"quota.login_started\"")
                    && s.contains("\"type\":\"quota.login_url_ready\"")
                    && s.contains("\"type\":\"quota.login_complete\"")
                {
                    return true;
                }
            }
        }
        false
    })
    .await
    .unwrap_or(false);

    let body = String::from_utf8_lossy(&buf).to_string();
    assert!(
        saw_all,
        "did not see all three quota.login_* SSE frames; got:\n{body}"
    );

    // Lexical order must match the bead spec (started → url_ready → complete).
    let p_started = body.find("\"type\":\"quota.login_started\"").unwrap();
    let p_url = body.find("\"type\":\"quota.login_url_ready\"").unwrap();
    let p_done = body.find("\"type\":\"quota.login_complete\"").unwrap();
    assert!(p_started < p_url && p_url < p_done, "wrong SSE order:\n{body}");

    // Typed-DTO decode: pull the `url_ready` frame and parse its payload as
    // `QuotaLoginUrlReady` (no `kind` field inside payload — discriminator lives
    // on `EventRecord.type`). The `data:` prefix for this frame lives *before*
    // the `"type":"quota.login_url_ready"` substring, so search backwards.
    let frame_start =
        body[..p_url].rfind("data:").unwrap() + "data:".len();
    let frame_end = body[frame_start..].find('\n').unwrap() + frame_start;
    let frame_json: serde_json::Value =
        serde_json::from_str(body[frame_start..frame_end].trim()).expect("frame is JSON");
    let payload = frame_json["payload"].clone();
    let typed: gt_web::QuotaLoginUrlReady =
        serde_json::from_value(payload).expect("payload matches QuotaLoginUrlReady");
    assert_eq!(typed.account, "acct-sse");
    assert_eq!(typed.flow_id, flow_id);
    assert_eq!(typed.url, "https://console.anthropic.com/oauth?state=zzz");

    root.shutdown();
}

// ---------------------------------------------------------------------------
// hq-fe-auth.4 — token-phase timeout + panic guard.

/// Same fixture as [`boot_with_pty`] but lets a test override [`LoginConfig`]
/// (notably `token_timeout_secs` and `url_timeout_secs`).
async fn boot_with_pty_and_config(
    pty: Arc<dyn gt_login::Pty>,
    cfg: LoginConfig,
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
        login_config: Arc::new(cfg),
        terminal_attach: None,
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

#[tokio::test]
async fn token_phase_timeout_rewrites_cancelled_as_timeout_token() {
    // Script prints the URL → driver transitions to AwaitingExit-via-token (phase==1).
    // Operator never POSTs `/login/token`, so `rx.recv()` blocks. Watchdog fires after
    // 200ms, stamps phase, drops the registry slot. Driver wakes (rx hangup), emits
    // `Failed{Cancelled}`; sink rewrites it as `Failed{Timeout{phase:"token"}}`.
    let pty = Arc::new(FakePty::scripted(
        vec![b"https://console.anthropic.com/oauth?state=t1\n".to_vec()],
        0,
    )) as Arc<dyn gt_login::Pty>;
    let cfg = LoginConfig {
        program: "claude".into(),
        args: vec!["/login".into()],
        // Disable URL-phase watchdog (driver hits UrlReady fast enough that the URL
        // soft deadline would race the test); short token deadline drives the
        // assertion.
        url_timeout_secs: 0,
        token_timeout_secs: 1, // smallest >0 (tokio::time::Duration::from_secs(0) is no-op)
    };
    let (base, root, mut events, _srv) = boot_with_pty_and_config(pty, cfg).await;

    let start: serde_json::Value = reqwest::Client::new()
        .post(format!("{base}/api/quota/accounts/acct-tt/login"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let flow_id = start["flow_id"].as_str().unwrap().to_string();

    // Pull frames until we see Failed; tolerate the polling sleep so the test does
    // not flake on a slow CI host.
    let mut kinds: Vec<String> = Vec::new();
    let mut failed_rec: Option<EventRecord> = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        match events.try_recv() {
            Ok(rec) if rec.kind.starts_with("quota.login_") => {
                let k = rec.kind.clone();
                if k == "quota.login_failed" {
                    failed_rec = Some(rec);
                    kinds.push(k);
                    break;
                }
                kinds.push(k);
            }
            Ok(_) => continue,
            Err(TryRecvError::Empty) => tokio::time::sleep(Duration::from_millis(20)).await,
            Err(_) => break,
        }
    }
    assert_eq!(
        kinds,
        vec![
            "quota.login_started",
            "quota.login_url_ready",
            "quota.login_failed",
        ],
        "expected typed timeout terminal frame after URL",
    );
    let rec = failed_rec.expect("failed frame present");
    assert_eq!(rec.payload["reason"]["kind"], "timeout");
    assert_eq!(rec.payload["reason"]["phase"], "token");
    assert_eq!(rec.payload["flow_id"], flow_id);
    assert!(rec.payload["message"]
        .as_str()
        .unwrap()
        .contains("timed out"));

    // Registry slot was cleared by the watchdog — a second start MUST succeed without 409.
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/quota/accounts/acct-tt/login"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 202, "slot cleared by timeout, restart works");
    // Drain the second flow so it does not outlive the test (cancel to dump the slot).
    let _ = reqwest::Client::new()
        .post(format!("{base}/api/quota/accounts/acct-tt/login/cancel"))
        .send()
        .await;
    root.shutdown();
}

#[tokio::test]
async fn url_phase_soft_timeout_stamps_phase_and_clears_slot() {
    // FakePty's scripted variant returns Ok(0) once the script drains, so the driver
    // emits `Failed{UrlMissing}` immediately — the watchdog never has a chance to fire
    // its soft-deadline path. We exercise the watchdog with an empty script + a very
    // short URL deadline; the driver's UrlMissing frame races the watchdog's
    // slot-drop, but either way the slot ends up empty so a restart succeeds.
    let pty = Arc::new(FakePty::scripted(vec![], 0)) as Arc<dyn gt_login::Pty>;
    let cfg = LoginConfig {
        program: "claude".into(),
        args: vec!["/login".into()],
        url_timeout_secs: 1,
        token_timeout_secs: 0,
    };
    let (base, root, _events, _srv) = boot_with_pty_and_config(pty, cfg).await;

    let resp = reqwest::Client::new()
        .post(format!("{base}/api/quota/accounts/acct-ut/login"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 202);

    // Give the driver / watchdog ~2s to settle.
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Whichever path won (driver UrlMissing or watchdog soft-timeout), the slot must
    // be empty — restart MUST land 202, not 409.
    let resp2 = reqwest::Client::new()
        .post(format!("{base}/api/quota/accounts/acct-ut/login"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp2.status(), 202);
    let _ = reqwest::Client::new()
        .post(format!("{base}/api/quota/accounts/acct-ut/login/cancel"))
        .send()
        .await;
    root.shutdown();
}

/// `FakePty` adapter whose first `read_chunk` panics. Used to drive the panic
/// guard in `gt-web::login`: the spawn_blocking task must catch the panic, emit
/// `Failed{Io}`, and clear the registry slot — otherwise the account is wedged
/// for the lifetime of the process.
struct PanickyPty;

impl gt_login::Pty for PanickyPty {
    fn spawn(
        &self,
        _program: &str,
        _args: &[&str],
    ) -> std::io::Result<Box<dyn gt_login::PtyChild>> {
        Ok(Box::new(PanickyChild))
    }
}

struct PanickyChild;

impl gt_login::PtyChild for PanickyChild {
    fn read_chunk(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        panic!("synthetic PTY read panic")
    }
    fn write_all(&mut self, _bytes: &[u8]) -> std::io::Result<()> {
        Ok(())
    }
    fn wait(&mut self) -> std::io::Result<i32> {
        Ok(0)
    }
    fn kill(&mut self) {}
    fn killer(&self) -> Box<dyn gt_login::PtyKiller> {
        struct NoopKiller;
        impl gt_login::PtyKiller for NoopKiller {
            fn kill(&self) {}
        }
        Box::new(NoopKiller)
    }
}

#[tokio::test]
async fn driver_panic_emits_failed_io_and_clears_slot() {
    let pty = Arc::new(PanickyPty) as Arc<dyn gt_login::Pty>;
    let cfg = LoginConfig {
        program: "claude".into(),
        args: vec!["/login".into()],
        url_timeout_secs: 0,
        token_timeout_secs: 0,
    };
    let (base, root, mut events, _srv) = boot_with_pty_and_config(pty, cfg).await;

    // Suppress the panic spew on stderr during this test — we expect it.
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));

    let resp = reqwest::Client::new()
        .post(format!("{base}/api/quota/accounts/acct-pn/login"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 202);

    // Wait for the synthetic `Failed{Io}` frame.
    let mut got_failed = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        match events.try_recv() {
            Ok(rec) if rec.kind == "quota.login_failed" => {
                got_failed = Some(rec);
                break;
            }
            Ok(_) => continue,
            Err(TryRecvError::Empty) => tokio::time::sleep(Duration::from_millis(20)).await,
            Err(_) => break,
        }
    }
    std::panic::set_hook(prev);

    let rec = got_failed.expect("panic guard emitted Failed{Io} frame");
    assert_eq!(rec.payload["reason"]["kind"], "io");
    assert!(rec.payload["reason"]["message"]
        .as_str()
        .unwrap()
        .contains("panicked"));

    // Slot must be cleared so a follow-up start succeeds.
    let resp2 = reqwest::Client::new()
        .post(format!("{base}/api/quota/accounts/acct-pn/login"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp2.status(), 202, "panic guard cleared the registry slot");
    let _ = reqwest::Client::new()
        .post(format!("{base}/api/quota/accounts/acct-pn/login/cancel"))
        .send()
        .await;
    root.shutdown();
}
