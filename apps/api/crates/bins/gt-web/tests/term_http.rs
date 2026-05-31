//! `GET /api/sessions/:id/term` gates (hq-fe-term.2). Two postures:
//!
//! - `terminal_attach: None` (default in deploys without `GT_TERMINAL_ENABLE=1`) — the
//!   route must short-circuit with 503 rather than attempting a WS upgrade against a
//!   missing backend. The dashboard reads the status to surface "terminal disabled".
//! - `terminal_attach: Some(FakeAttach)` — the upgrade path resolves through axum's
//!   `WebSocketUpgrade` extractor; without the `Upgrade: websocket` headers the request
//!   should reach the extractor and fail with 426/400 (axum's extractor rejection) — i.e.
//!   we get past the 503 short-circuit.
//!
//! Full duplex byte-flow coverage is out of scope here — a true WS handshake test would
//! need `tokio-tungstenite` and a real upgrade. Tracked as a follow-up.

use std::sync::Arc;

use tokio::net::TcpListener;

use gt_agent::InMemorySessions;
use gt_beads::InMemoryBeads;
use gt_root::{root::Effects, spawn, RootConfig, SystemClock};
use gt_terminal::FakeAttach;
use gt_web::{router, AppState, AuthConfig, InMemoryWebAudit, ReadinessGate, WebAuditSink};

struct NoopEffects;
impl Effects for NoopEffects {
    fn sling(&self, _convoy: &str, _member: &str) {}
    fn rotate(&self, _account: &str) {}
}

fn temp_log() -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("gt-web-term-log-{}.jsonl", ulid::Ulid::new()));
    p
}

fn build_state(
    attach: Option<Arc<dyn gt_terminal::Attach>>,
) -> AppState<InMemoryBeads, InMemorySessions, gt_merge::InMemoryMergeRepo> {
    let beads = Arc::new(InMemoryBeads::default());
    let sessions = Arc::new(InMemorySessions::default());
    let merges = Arc::new(gt_merge::InMemoryMergeRepo::default());
    let root = spawn(
        beads.clone(),
        merges.clone(),
        Arc::new(gt_patrol::InMemoryPatrolRepo::default()),
        Arc::new(gt_orchestration::InMemoryOrchRepo::default()),
        NoopEffects,
        SystemClock,
        temp_log(),
        RootConfig::default(),
    );
    AppState {
        beads,
        sessions,
        merges,
        agent_events: root.agent_events.clone(),
        events: root.events_sender(),
        town_root: None,
        issues: None,
        bus: None,
        worktrees_stream: None,
        control: None,
        respawner: None,
        commenter: None,
        event_log: None,
        login_registry: Arc::new(gt_web::LoginRegistry::new()),
        login_pty: None,
        login_config: Arc::new(gt_web::LoginConfig::default()),
        terminal_attach: attach,
        skills: None,
    }
}

async fn spawn_router(
    state: AppState<InMemoryBeads, InMemorySessions, gt_merge::InMemoryMergeRepo>,
) -> std::net::SocketAddr {
    let sink: Arc<dyn WebAuditSink> = Arc::new(InMemoryWebAudit::new());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state, AuthConfig::open(), sink, ReadinessGate::ready());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    addr
}

/// When `terminal_attach` is unset (deploy without `GT_TERMINAL_ENABLE=1`), the route
/// must short-circuit with 503 *before* attempting any WS upgrade. The body explains
/// the env gate so an operator can debug from logs without source.
#[tokio::test]
async fn term_returns_503_when_attach_unwired() {
    let state = build_state(None);
    let addr = spawn_router(state).await;
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/api/sessions/polecat-x/term"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("terminal attach not wired"),
        "body must surface env-gate hint, got: {body}"
    );
}

/// When the attach adapter is wired but the caller did not request a WebSocket upgrade,
/// the request falls through to `WebSocketUpgrade`'s extractor rejection. We don't
/// pin the exact status (axum's WebSocketUpgrade returns 426 / 400 depending on what
/// header is missing) — the important property is "not 503", which proves the route
/// hands off to the WS extractor instead of short-circuiting.
#[tokio::test]
async fn term_attempts_ws_upgrade_when_attach_wired() {
    let state = build_state(Some(Arc::new(FakeAttach::new())));
    let addr = spawn_router(state).await;
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/api/sessions/polecat-x/term"))
        .send()
        .await
        .unwrap();
    assert_ne!(
        resp.status(),
        503,
        "wired adapter must reach the WS extractor (got 503, means short-circuit)"
    );
    assert!(
        resp.status().is_client_error() || resp.status().is_server_error(),
        "non-upgrade GET must be rejected by WebSocketUpgrade extractor, got {}",
        resp.status()
    );
}
