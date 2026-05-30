//! `GET /api/issues` gate (hq-fe-api-r.9). The handler short-circuits to an empty list when
//! `AppState.issues` is `None` so the in-memory dev mode keeps working without Dolt — same
//! posture as `/api/worktrees`. The populated path needs a real Dolt connection; that
//! contract is covered by the `gt-store-dolt` `issues_repo` integration tests behind
//! `GT_DOLT_URL`, kept off this fast unit-style suite.

use std::sync::Arc;

use tokio::net::TcpListener;

use gt_agent::InMemorySessions;
use gt_beads::InMemoryBeads;
use gt_root::{root::Effects, spawn, RootConfig, SystemClock};
use gt_web::dto::IssueDto;
use gt_web::{router, AppState, AuthConfig, InMemoryWebAudit, ReadinessGate, WebAuditSink};

struct NoopEffects;
impl Effects for NoopEffects {
    fn sling(&self, _convoy: &str, _member: &str) {}
    fn rotate(&self, _account: &str) {}
}

#[tokio::test]
async fn empty_when_issues_unset() {
    let beads = Arc::new(InMemoryBeads::default());
    let sessions = Arc::new(InMemorySessions::default());
    let log = {
        let mut p = std::env::temp_dir();
        p.push(format!("gt-web-issues-test-{}.jsonl", ulid::Ulid::new()));
        p
    };
    let root = spawn(
        beads.clone(),
        Arc::new(gt_merge::InMemoryMergeRepo::default()),
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
    let app = router(state, AuthConfig::open(), sink, ReadinessGate::ready());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let rows: Vec<IssueDto> = reqwest::Client::new()
        .get(format!("http://{addr}/api/issues?status=working"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(rows.is_empty(), "expected empty issues list, got {rows:?}");
}
