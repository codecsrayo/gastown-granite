//! hq-fe-rbac.4: `GET /api/whoami` surfaces the request actor + frontier auth mode.
//! Open mode → `actor=web:open`, `mode=open`; bearer mode → `actor=web:<sha-prefix>`,
//! `mode=bearer`. Pre-RBAC roles/scopes arrays come back empty but are on the wire so
//! the FE can hydrate against a stable contract.

use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;

use gt_agent::InMemorySessions;
use gt_beads::InMemoryBeads;
use gt_root::{root::Effects, spawn, RootConfig, RootHandle, SystemClock};
use gt_web::{
    auth::actor_tag, router, AppState, AuthConfig, InMemoryWebAudit, ReadinessGate, WebAuditSink,
};

struct NoopEffects;
impl Effects for NoopEffects {
    fn sling(&self, _convoy: &str, _member: &str) {}
    fn rotate(&self, _account: &str) {}
}

async fn boot(auth: AuthConfig) -> (String, RootHandle<Arc<InMemoryBeads>>) {
    let beads = Arc::new(InMemoryBeads::default());
    let sessions = Arc::new(InMemorySessions::new(vec![]));
    let merges = Arc::new(gt_merge::InMemoryMergeRepo::default());
    let log = std::env::temp_dir().join(format!("gt-web-whoami-{}.jsonl", ulid::Ulid::new()));
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

#[tokio::test]
async fn whoami_open_mode_surfaces_dev_actor() {
    let (base, root) = boot(AuthConfig::open()).await;
    let client = reqwest::Client::new();

    let body: gt_web::dto::WhoamiDto = client
        .get(format!("{base}/api/whoami"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body.actor, "web:open");
    assert_eq!(body.mode, "open");
    assert!(body.roles.is_empty());
    assert!(body.scopes.is_empty());

    tokio::time::sleep(Duration::from_millis(10)).await;
    root.shutdown();
}

#[tokio::test]
async fn whoami_bearer_mode_returns_actor_tag() {
    let secret = "topsecret-bearer-token";
    let (base, root) = boot(AuthConfig::bearer(secret)).await;
    let client = reqwest::Client::new();

    let body: gt_web::dto::WhoamiDto = client
        .get(format!("{base}/api/whoami"))
        .header("authorization", format!("Bearer {secret}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body.actor, actor_tag(secret));
    assert_eq!(body.mode, "bearer");
    assert!(body.roles.is_empty());

    tokio::time::sleep(Duration::from_millis(10)).await;
    root.shutdown();
}

#[tokio::test]
async fn whoami_bearer_mode_rejects_missing_header() {
    let (base, root) = boot(AuthConfig::bearer("topsecret")).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base}/api/whoami"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    tokio::time::sleep(Duration::from_millis(10)).await;
    root.shutdown();
}
