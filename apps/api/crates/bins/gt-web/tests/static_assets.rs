//! hq-fe-cut.1 — static asset serving. The SvelteKit build lives in `apps/web/build`;
//! `gt-web` mounts it as the router fallback so `/` serves `index.html`, `/_app/*`
//! serves the hashed bundles, and any SPA history-mode path (`/sessions/abc`) falls
//! back to `index.html` for the client router. Static assets sit **outside** the auth
//! middleware: the login page is part of the SPA and must load without a bearer.
//!
//! Invariants checked:
//! 1. `GET /` → 200 with the bundled `index.html` body.
//! 2. `GET /_app/immutable/<asset>` → 200 with the bundled bytes (asset-hash routing).
//! 3. `GET /unknown/spa/path` → 200 with `index.html` (SPA history-mode fallback).
//! 4. `GET /api/sessions` without a token → still 401 (auth not lifted by the fallback).

use std::sync::Arc;

use tokio::net::TcpListener;

use gt_agent::InMemorySessions;
use gt_beads::InMemoryBeads;
use gt_root::{root::Effects, spawn, RootConfig, SystemClock};
use gt_web::{
    router, with_static_assets, AppState, AuthConfig, InMemoryWebAudit, ReadinessGate,
    WebAuditSink,
};

struct NoopEffects;
impl Effects for NoopEffects {
    fn sling(&self, _convoy: &str, _member: &str) {}
    fn rotate(&self, _account: &str) {}
}

fn tempfile(stem: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("gt-web-static-{stem}-{}.jsonl", ulid::Ulid::new()))
}

fn tempdist() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("gt-web-dist-{}", ulid::Ulid::new()));
    let app = dir.join("_app").join("immutable");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(
        dir.join("index.html"),
        b"<!doctype html><html><body data-test=\"spa-shell\"></body></html>",
    )
    .unwrap();
    std::fs::write(app.join("entry.js"), b"export const v = 42;\n").unwrap();
    dir
}

async fn boot(dist: std::path::PathBuf) -> String {
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
        tempfile("log"),
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
    };

    let audit = InMemoryWebAudit::new();
    let sink: Arc<dyn WebAuditSink> = Arc::new(audit);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state, AuthConfig::bearer("s3cret"), sink, ReadinessGate::ready());
    let app = with_static_assets(app, &dist);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    std::mem::forget(root);
    format!("http://{addr}")
}

#[tokio::test]
async fn serves_index_at_root() {
    let dist = tempdist();
    let base = boot(dist.clone()).await;
    let res = reqwest::get(format!("{base}/")).await.unwrap();
    assert_eq!(res.status(), 200);
    let body = res.text().await.unwrap();
    assert!(body.contains("spa-shell"), "expected SPA shell, got {body}");
}

#[tokio::test]
async fn serves_hashed_asset_under_app_immutable() {
    let dist = tempdist();
    let base = boot(dist.clone()).await;
    let res = reqwest::get(format!("{base}/_app/immutable/entry.js"))
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body = res.text().await.unwrap();
    assert!(body.contains("const v = 42"), "got {body}");
}

#[tokio::test]
async fn spa_history_fallback_returns_index() {
    // SvelteKit client router owns `/sessions/<id>`; the server must not 404 on first paint.
    let dist = tempdist();
    let base = boot(dist.clone()).await;
    let res = reqwest::get(format!("{base}/sessions/abc-123"))
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body = res.text().await.unwrap();
    assert!(body.contains("spa-shell"), "expected SPA shell, got {body}");
}

#[tokio::test]
async fn api_routes_still_require_auth_with_assets_mounted() {
    let dist = tempdist();
    let base = boot(dist.clone()).await;
    let res = reqwest::get(format!("{base}/api/sessions")).await.unwrap();
    assert_eq!(res.status(), 401);
}

#[tokio::test]
async fn missing_dist_leaves_router_untouched() {
    // No dist on disk — fallback should still return 404 for `/`, not panic at construction.
    let dist = std::env::temp_dir().join(format!("gt-web-nodist-{}", ulid::Ulid::new()));
    let base = boot(dist).await;
    let res = reqwest::get(format!("{base}/")).await.unwrap();
    assert_eq!(res.status(), 404);
}
