//! hq-fe-api-r.5: `GET /api/feed?since=&limit=` historical replay over the shared
//! `events.jsonl`. Three fixtures: (a) tail without `since` returns the whole log in
//! file order; (b) `since=<ts>` returns strict-after; (c) `limit` caps the response and
//! keeps the most-recent suffix.

use std::io::Write;
use std::sync::Arc;

use tokio::net::TcpListener;

use gt_agent::InMemorySessions;
use gt_audit::EventRecord;
use gt_beads::InMemoryBeads;
use gt_root::{root::Effects, spawn, RootConfig, RootHandle, SystemClock};
use gt_web::{router, AppState, AuthConfig, InMemoryWebAudit, ReadinessGate, WebAuditSink};

struct NoopEffects;
impl Effects for NoopEffects {
    fn sling(&self, _convoy: &str, _member: &str) {}
    fn rotate(&self, _account: &str) {}
}

fn rec(event_id: &str, ts: &str, kind: &str) -> EventRecord {
    EventRecord {
        event_id: event_id.into(),
        correlation_id: "corr-1".into(),
        causation_id: None,
        ts: ts.into(),
        kind: kind.into(),
        payload: serde_json::json!({ "msg": kind }),
    }
}

async fn boot(records: &[EventRecord]) -> (String, RootHandle<Arc<InMemoryBeads>>, std::path::PathBuf) {
    let log = std::env::temp_dir().join(format!("gt-web-feed-{}.jsonl", ulid::Ulid::new()));
    {
        let mut f = std::fs::File::create(&log).expect("create log");
        for r in records {
            let line = serde_json::to_string(r).unwrap();
            writeln!(f, "{}", line).unwrap();
        }
    }

    let beads = Arc::new(InMemoryBeads::default());
    let sessions = Arc::new(InMemorySessions::new(vec![]));
    let reactor_log = std::env::temp_dir().join(format!("gt-web-feed-react-{}.jsonl", ulid::Ulid::new()));
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
        bus: None,
        worktrees_stream: None,
        control: None,
        respawner: None,
        commenter: None,
        event_log: Some(Arc::new(log.clone())),
    };
    let sink: Arc<dyn WebAuditSink> = Arc::new(InMemoryWebAudit::new());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state, AuthConfig::open(), sink, ReadinessGate::ready());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), root, log)
}

#[tokio::test]
async fn feed_returns_full_tail_without_since() {
    let fixture = vec![
        rec("e1", "2026-05-29T10:00:00Z", "agent.spawned"),
        rec("e2", "2026-05-29T10:00:01Z", "agent.heartbeat"),
        rec("e3", "2026-05-29T10:00:02Z", "merge.queued"),
    ];
    let (base, root, _log) = boot(&fixture).await;

    let body: Vec<EventRecord> = reqwest::get(format!("{base}/api/feed"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body.len(), 3);
    assert_eq!(body[0].event_id, "e1");
    assert_eq!(body[2].event_id, "e3");
    root.shutdown();
}

#[tokio::test]
async fn feed_since_filters_strict_after() {
    let fixture = vec![
        rec("e1", "2026-05-29T10:00:00Z", "agent.spawned"),
        rec("e2", "2026-05-29T10:00:01Z", "agent.heartbeat"),
        rec("e3", "2026-05-29T10:00:02Z", "merge.queued"),
    ];
    let (base, root, _log) = boot(&fixture).await;

    let body: Vec<EventRecord> = reqwest::get(format!(
        "{base}/api/feed?since=2026-05-29T10:00:01Z"
    ))
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(body.len(), 1);
    assert_eq!(body[0].event_id, "e3");
    root.shutdown();
}

#[tokio::test]
async fn feed_limit_caps_to_recent_suffix() {
    let fixture = vec![
        rec("e1", "2026-05-29T10:00:00Z", "k.a"),
        rec("e2", "2026-05-29T10:00:01Z", "k.b"),
        rec("e3", "2026-05-29T10:00:02Z", "k.c"),
        rec("e4", "2026-05-29T10:00:03Z", "k.d"),
    ];
    let (base, root, _log) = boot(&fixture).await;

    let body: Vec<EventRecord> = reqwest::get(format!("{base}/api/feed?limit=2"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body.len(), 2);
    assert_eq!(body[0].event_id, "e3");
    assert_eq!(body[1].event_id, "e4");
    root.shutdown();
}

#[tokio::test]
async fn feed_empty_when_event_log_unset() {
    let beads = Arc::new(InMemoryBeads::default());
    let sessions = Arc::new(InMemorySessions::new(vec![]));
    let log = std::env::temp_dir().join(format!("gt-web-feed-none-{}.jsonl", ulid::Ulid::new()));
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
    };
    let sink: Arc<dyn WebAuditSink> = Arc::new(InMemoryWebAudit::new());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state, AuthConfig::open(), sink, ReadinessGate::ready());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let base = format!("http://{addr}");

    let body: Vec<EventRecord> = reqwest::get(format!("{base}/api/feed"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(body.is_empty());
    root.shutdown();
}
