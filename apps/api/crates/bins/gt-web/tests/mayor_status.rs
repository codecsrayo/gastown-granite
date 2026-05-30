//! hq-fe-api-r.7: `GET /api/mayor/status` derives mayor attach from the live session
//! registry. Two fixtures: (a) a mayor is present → `attached=true` with id/rig/state
//! surfaced; (b) only dogs are present → `attached=false` with all other fields null.

use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;

use gt_agent::{DogKind, InMemorySessions, Session, SessionRole};
use gt_beads::InMemoryBeads;
use gt_root::{root::Effects, spawn, RootConfig, RootHandle, SystemClock};
use gt_web::{router, AppState, AuthConfig, InMemoryWebAudit, ReadinessGate, WebAuditSink};

struct NoopEffects;
impl Effects for NoopEffects {
    fn sling(&self, _convoy: &str, _member: &str) {}
    fn rotate(&self, _account: &str) {}
}

async fn boot(sessions: Vec<Session>) -> (String, RootHandle<Arc<InMemoryBeads>>) {
    let beads = Arc::new(InMemoryBeads::default());
    let sessions = Arc::new(InMemorySessions::new(sessions));
    let log = std::env::temp_dir().join(format!("gt-web-mayor-{}.jsonl", ulid::Ulid::new()));
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
    };
    let sink: Arc<dyn WebAuditSink> = Arc::new(InMemoryWebAudit::new());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state, AuthConfig::open(), sink, ReadinessGate::ready());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), root)
}

#[tokio::test]
async fn mayor_status_attached_when_mayor_session_present() {
    let fixture = vec![
        Session::with_role("mayor", "town", SessionRole::Mayor, None),
        Session::with_role(
            "gt-granite-witness",
            "granite",
            SessionRole::Dog(DogKind::Witness),
            None,
        ),
    ];
    let (base, root) = boot(fixture).await;
    let client = reqwest::Client::new();

    let body: gt_web::dto::MayorStatusDto = client
        .get(format!("{base}/api/mayor/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(body.attached);
    assert_eq!(body.session_id.as_deref(), Some("mayor"));
    assert_eq!(body.rig.as_deref(), Some("town"));
    assert_eq!(body.state.as_deref(), Some("spawned"));

    tokio::time::sleep(Duration::from_millis(10)).await;
    root.shutdown();
}

#[tokio::test]
async fn mayor_status_detached_when_only_dogs_present() {
    let fixture = vec![
        Session::with_role(
            "gt-granite-witness",
            "granite",
            SessionRole::Dog(DogKind::Witness),
            None,
        ),
        Session::with_role(
            "gt-granite-atom",
            "granite",
            SessionRole::Polecat,
            Some("atom".into()),
        ),
    ];
    let (base, root) = boot(fixture).await;
    let client = reqwest::Client::new();

    let body: gt_web::dto::MayorStatusDto = client
        .get(format!("{base}/api/mayor/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!body.attached);
    assert_eq!(body.session_id, None);
    assert_eq!(body.rig, None);
    assert_eq!(body.state, None);

    tokio::time::sleep(Duration::from_millis(10)).await;
    root.shutdown();
}

#[tokio::test]
async fn mayor_status_detached_when_no_sessions() {
    let (base, root) = boot(vec![]).await;
    let client = reqwest::Client::new();

    let body: gt_web::dto::MayorStatusDto = client
        .get(format!("{base}/api/mayor/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!body.attached);
    assert_eq!(body.session_id, None);

    tokio::time::sleep(Duration::from_millis(10)).await;
    root.shutdown();
}
