//! End-to-end coverage for hq-fe-rbac.3 — per-route scope guards on the live router.
//!
//! The middleware unit tests in `src/scope.rs` cover the gate logic in isolation; this
//! file pins the wiring contract: each `/api/*` route the dashboard hits actually has the
//! expected scope guard attached. We probe a handful of representative routes in JWT mode
//! against a strangler token (no scopes) and assert 403, then re-probe with a properly
//! provisioned token and assert 200/empty-list passthrough.
//!
//! `/api/whoami` is the deliberate exception: it never requires a scope so the dashboard
//! can hydrate identity for any authenticated actor.

use std::sync::Arc;

use tokio::net::TcpListener;

use gt_agent::InMemorySessions;
use gt_beads::InMemoryBeads;
use gt_root::{root::Effects, spawn, RootConfig, RootHandle, SystemClock};
use gt_web::{
    router, AppState, AuthConfig, InMemoryWebAudit, JwtIssuer, ReadinessGate, WebAuditEvent,
    WebAuditSink,
};

struct NoopEffects;
impl Effects for NoopEffects {
    fn sling(&self, _convoy: &str, _member: &str) {}
    fn rotate(&self, _account: &str) {}
}

async fn boot_jwt() -> (String, Arc<JwtIssuer>, Arc<InMemoryWebAudit>, RootHandle<Arc<InMemoryBeads>>) {
    let beads = Arc::new(InMemoryBeads::default());
    let sessions = Arc::new(InMemorySessions::new(vec![]));
    let merges = Arc::new(gt_merge::InMemoryMergeRepo::default());
    let log = std::env::temp_dir().join(format!("gt-web-scope-{}.jsonl", ulid::Ulid::new()));
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
        bus: None,
        worktrees_stream: None,
        control: None,
        respawner: None,
        commenter: None,
        event_log: None,
        login_registry: std::sync::Arc::new(gt_web::LoginRegistry::new()),
        login_pty: None,
        login_config: std::sync::Arc::new(gt_web::LoginConfig::default()),
         terminal_attach: None,
         skills: None,
    };
    let issuer = JwtIssuer::from_secret("rbac-3-test-secret").shared();
    let audit = Arc::new(InMemoryWebAudit::new());
    let sink: Arc<dyn WebAuditSink> = audit.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(
        state,
        AuthConfig::jwt(issuer.clone()),
        sink,
        ReadinessGate::ready(),
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), issuer, audit, root)
}

/// A token with no scopes — every guarded route should reject it; only `/api/whoami` lets
/// it through.
#[tokio::test]
async fn no_scopes_token_is_forbidden_on_every_guarded_route_and_audited() {
    let (base, issuer, audit, _root) = boot_jwt().await;
    let token = issuer
        .sign("stranger", vec![], vec![])
        .expect("sign empty-scope token");
    let client = reqwest::Client::new();

    // Each pair: (route, expected scope) — the route key the dashboard uses + the
    // capability `lib.rs` declares for it. Mismatch here = mismatch in production.
    let cases = [
        ("/api/sessions", "sessions.read"),
        ("/api/beads?status=pending", "beads.read"),
        ("/api/issues", "beads.read"),
        ("/api/merges", "merge.read"),
        ("/api/convoys", "convoys.read"),
        ("/api/feed", "feed.read"),
        ("/api/quota/rotation", "quota.read"),
        ("/api/mayor/status", "sessions.read"),
        ("/api/worktrees", "worktrees.read"),
    ];

    for (path, scope) in cases {
        let resp = client
            .get(format!("{base}{path}"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::FORBIDDEN,
            "{path} should return 403 without {scope}"
        );
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["scope"], scope, "{path} should report missing scope");
    }

    // whoami stays open to any verified actor — identity bootstrap.
    let whoami = client
        .get(format!("{base}/api/whoami"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(whoami.status(), reqwest::StatusCode::OK);

    // Every Forbidden audit record carries the actor sub + the missing scope. The audit
    // sink saw at least one entry per guarded route above.
    let forbidden_count = audit
        .snapshot()
        .iter()
        .filter(|e| matches!(e, WebAuditEvent::Forbidden { actor, .. } if actor == "stranger"))
        .count();
    assert!(
        forbidden_count >= 9,
        "expected at least 9 Forbidden audit records, got {forbidden_count}"
    );
}

/// A token carrying the full read suite should hit every read route without 403. Each
/// route's empty-state response (`[]`, default DTO) is enough — we only check that the
/// guard does not interpose.
#[tokio::test]
async fn properly_scoped_token_passes_every_read_route() {
    let (base, issuer, audit, _root) = boot_jwt().await;
    let token = issuer
        .sign(
            "reader",
            vec!["reader".into()],
            vec![
                "sessions.read".into(),
                "beads.read".into(),
                "merge.read".into(),
                "convoys.read".into(),
                "feed.read".into(),
                "quota.read".into(),
                "worktrees.read".into(),
            ],
        )
        .expect("sign reader token");
    let client = reqwest::Client::new();

    for path in [
        "/api/sessions",
        "/api/beads?status=pending",
        "/api/issues",
        "/api/merges",
        "/api/convoys",
        "/api/feed",
        "/api/quota/rotation",
        "/api/mayor/status",
        "/api/worktrees",
        "/api/whoami",
    ] {
        let resp = client
            .get(format!("{base}{path}"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::OK,
            "{path} should pass with read suite"
        );
    }

    // None of the calls above produced a Forbidden record.
    let any_forbidden = audit
        .snapshot()
        .iter()
        .any(|e| matches!(e, WebAuditEvent::Forbidden { .. }));
    assert!(!any_forbidden, "no Forbidden audit expected with full read suite");
}

/// Read-only token must still be denied on write routes; the per-route guard isolates
/// `beads.read` from `beads.write` so a leaked viewer token cannot mutate.
#[tokio::test]
async fn read_only_token_cannot_drive_write_routes() {
    let (base, issuer, _audit, _root) = boot_jwt().await;
    let token = issuer
        .sign("reader", vec![], vec!["beads.read".into()])
        .expect("sign reader token");
    let client = reqwest::Client::new();

    let body = serde_json::json!({
        "id": "test-bead",
        "title": "should not land",
        "priority": 2,
    });
    let resp = client
        .post(format!("{base}/api/beads"))
        .bearer_auth(&token)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::FORBIDDEN);
    let payload: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(payload["scope"], "beads.write");
}

/// Bearer mode predates RBAC and must keep working: a valid shared-secret request hits
/// the handler regardless of the per-route guard.
#[tokio::test]
async fn bearer_mode_grandfathers_through_guarded_routes() {
    let beads = Arc::new(InMemoryBeads::default());
    let sessions = Arc::new(InMemorySessions::new(vec![]));
    let merges = Arc::new(gt_merge::InMemoryMergeRepo::default());
    let log = std::env::temp_dir().join(format!("gt-web-scope-bearer-{}.jsonl", ulid::Ulid::new()));
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
        bus: None,
        worktrees_stream: None,
        control: None,
        respawner: None,
        commenter: None,
        event_log: None,
        login_registry: std::sync::Arc::new(gt_web::LoginRegistry::new()),
        login_pty: None,
        login_config: std::sync::Arc::new(gt_web::LoginConfig::default()),
         terminal_attach: None,
         skills: None,
    };
    let sink: Arc<dyn WebAuditSink> = Arc::new(InMemoryWebAudit::new());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(
        state,
        AuthConfig::bearer("legacy-tok"),
        sink,
        ReadinessGate::ready(),
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/sessions"))
        .bearer_auth("legacy-tok")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
}
