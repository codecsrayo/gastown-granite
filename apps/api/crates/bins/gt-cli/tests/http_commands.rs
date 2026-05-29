//! Integration gate for gt-cli Phase 1 (hq-hapx). Boots the real `gt-web` router in-process
//! on a transient socket — the same harness shape as `gt-web/tests/e2e.rs` — and drives the
//! CLI's typed fetchers against it. Proves the wire contract (DTO shapes, `?role=`/`?status=`
//! filters, bearer auth) the CLI depends on actually holds against the live server.

use std::sync::Arc;

use gt_agent::{InMemorySessions, Session, SessionRole, SessionWriter};
use gt_beads::{Bead, BeadRepository, BeadStatus, InMemoryBeads};
use gt_root::{root::Effects, spawn, RootConfig, RootHandle, SystemClock};
use gt_web::{router, AppState, AuthConfig, InMemoryWebAudit, ReadinessGate, WebAuditSink};
use tokio::net::TcpListener;

use gt_cli::{fetch_beads, fetch_sessions, nudge, Config};

struct NoopEffects;
impl Effects for NoopEffects {
    fn sling(&self, _convoy: &str, _member: &str) {}
    fn rotate(&self, _account: &str) {}
}

/// Boot gt-web with one pending bead and two sessions (a polecat + a mayor). Returns the base
/// URL and the live root (kept alive by the caller so the server keeps serving).
async fn boot(auth: AuthConfig) -> (String, RootHandle<Arc<InMemoryBeads>>) {
    let beads = Arc::new(InMemoryBeads::default());
    beads
        .upsert(&Bead::new("hq-x1", "demo bead", BeadStatus::Pending, 1))
        .await
        .unwrap();

    let sessions = Arc::new(InMemorySessions::default());
    let mut polecat = Session::new("granite-toast", "granite");
    polecat.role = SessionRole::Polecat;
    sessions.upsert(&polecat).await.unwrap();
    let mut mayor = Session::new("hq-mayor", "hq");
    mayor.role = SessionRole::Mayor;
    sessions.upsert(&mayor).await.unwrap();

    let root = spawn(
        beads.clone(),
        Arc::new(gt_merge::InMemoryMergeRepo::default()),
        Arc::new(gt_patrol::InMemoryPatrolRepo::default()),
        Arc::new(gt_orchestration::InMemoryOrchRepo::default()),
        NoopEffects,
        SystemClock,
        tempfile(),
        RootConfig::default(),
    );

    let state = AppState {
        beads,
        sessions,
        agent_events: root.agent_events.clone(),
        events: root.events_sender(),
    };
    let sink: Arc<dyn WebAuditSink> = Arc::new(InMemoryWebAudit::new());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state, auth, sink, ReadinessGate::ready());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), root)
}

fn tempfile() -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("gt-cli-test-{}.jsonl", ulid::Ulid::new()));
    p
}

#[tokio::test]
async fn agents_lists_all_and_filters_by_role() {
    let (base, _root) = boot(AuthConfig::open()).await;
    let cfg = Config::new(base, None);
    let client = reqwest::Client::new();

    let all = fetch_sessions(&client, &cfg, None).await.unwrap();
    assert_eq!(all.len(), 2, "expected both seeded sessions");

    let polecats = fetch_sessions(&client, &cfg, Some("polecat")).await.unwrap();
    assert_eq!(polecats.len(), 1);
    assert_eq!(polecats[0].id, "granite-toast");
    assert_eq!(polecats[0].role, "polecat");

    let mayors = fetch_sessions(&client, &cfg, Some("mayor")).await.unwrap();
    assert_eq!(mayors.len(), 1);
    assert_eq!(mayors[0].id, "hq-mayor");

    // Unknown role is a view that matches nothing, not an error.
    let none = fetch_sessions(&client, &cfg, Some("nope")).await.unwrap();
    assert!(none.is_empty());
}

#[tokio::test]
async fn beads_by_status_and_bad_status_errors() {
    let (base, _root) = boot(AuthConfig::open()).await;
    let cfg = Config::new(base, None);
    let client = reqwest::Client::new();

    let pending = fetch_beads(&client, &cfg, "pending").await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, "hq-x1");
    assert_eq!(pending[0].status, "pending");

    // A valid-but-empty status returns an empty list (not an error).
    let done = fetch_beads(&client, &cfg, "done").await.unwrap();
    assert!(done.is_empty());

    // Unknown status -> server returns 400 -> fetch surfaces an error (not a silent empty).
    let bad = fetch_beads(&client, &cfg, "bogus").await;
    assert!(bad.is_err(), "bogus status must error, got {bad:?}");
}

#[tokio::test]
async fn heartbeat_is_accepted() {
    let (base, _root) = boot(AuthConfig::open()).await;
    let cfg = Config::new(base, None);
    let client = reqwest::Client::new();

    let accepted = nudge(&client, &cfg, "granite-toast").await.unwrap();
    assert!(accepted, "nudge should be accepted by the relay");
}

#[tokio::test]
async fn bearer_auth_required_when_configured() {
    let token = "shared-test-token";
    let (base, _root) = boot(AuthConfig::bearer(token.to_string())).await;
    let client = reqwest::Client::new();

    // Correct token -> ok.
    let ok_cfg = Config::new(base.clone(), Some(token.to_string()));
    let beads = fetch_beads(&client, &ok_cfg, "pending").await.unwrap();
    assert_eq!(beads.len(), 1);

    // Wrong token -> 401 -> error.
    let bad_cfg = Config::new(base.clone(), Some("wrong".to_string()));
    assert!(fetch_beads(&client, &bad_cfg, "pending").await.is_err());

    // No token -> 401 -> error.
    let no_cfg = Config::new(base, None);
    assert!(fetch_beads(&client, &no_cfg, "pending").await.is_err());
}
