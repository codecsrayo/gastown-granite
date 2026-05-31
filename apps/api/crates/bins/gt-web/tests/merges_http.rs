//! hq-fe-api-r.4: `GET /api/merges` returns the merge slot board as a stable snapshot.
//!
//! Seeds an in-memory `MergeRepository` with three slots in distinct lifecycle states,
//! then asserts the route surfaces them in `bead`-id order with the canonical state
//! string flattening (`ready|merging|merged|failed`). A second fixture asserts the
//! empty-board case so the dashboard can distinguish "no merges" from "endpoint
//! missing" without a 404.

use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;

use gt_agent::InMemorySessions;
use gt_beads::InMemoryBeads;
use gt_merge::{InMemoryMergeRepo, MergeRepository, MergeSlot, MergeSlotState};
use gt_root::{root::Effects, spawn, RootConfig, RootHandle, SystemClock};
use gt_web::{router, AppState, AuthConfig, InMemoryWebAudit, ReadinessGate, WebAuditSink};

struct NoopEffects;
impl Effects for NoopEffects {
    fn sling(&self, _convoy: &str, _member: &str) {}
    fn rotate(&self, _account: &str) {}
}

async fn boot(seed: Vec<MergeSlot>) -> (String, RootHandle<Arc<InMemoryBeads>>) {
    let beads = Arc::new(InMemoryBeads::default());
    let sessions = Arc::new(InMemorySessions::new(vec![]));
    let merges = Arc::new(InMemoryMergeRepo::default());
    for slot in &seed {
        merges.upsert_slot(slot).await.expect("seed slot");
    }
    let log = std::env::temp_dir().join(format!("gt-web-merges-{}.jsonl", ulid::Ulid::new()));
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
    let app = router(state, AuthConfig::open(), sink, ReadinessGate::ready());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), root)
}

#[tokio::test]
async fn api_merges_returns_seeded_board_sorted_by_bead() {
    // Three slots covering ready/merging/failed. `merged` transitions through `merging`
    // first; build one manually to exercise the state string mapping without driving the
    // actor (the route is read-only so the wire shape is what matters here).
    let mut working = MergeSlot::new("hq-bead-b", "claim/hq-bead-b");
    working.transition(MergeSlotState::Merging).unwrap();
    let mut merged = MergeSlot::new("hq-bead-c", "claim/hq-bead-c");
    merged.transition(MergeSlotState::Merging).unwrap();
    merged.transition(MergeSlotState::Merged).unwrap();
    let seed = vec![
        MergeSlot::new("hq-bead-a", "claim/hq-bead-a"),
        working,
        merged,
    ];

    let (base, root) = boot(seed).await;
    let client = reqwest::Client::new();

    let body: Vec<gt_web::dto::MergeSlotDto> = client
        .get(format!("{base}/api/merges"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body.len(), 3, "three slots seeded");
    // InMemoryMergeRepo iterates BTreeMap → stable sort by bead id.
    let beads: Vec<&str> = body.iter().map(|s| s.bead.as_str()).collect();
    assert_eq!(beads, vec!["hq-bead-a", "hq-bead-b", "hq-bead-c"]);

    let by_bead = |id: &str| body.iter().find(|s| s.bead == id).unwrap();
    assert_eq!(by_bead("hq-bead-a").state, "ready");
    assert_eq!(by_bead("hq-bead-a").branch, "claim/hq-bead-a");
    assert_eq!(by_bead("hq-bead-b").state, "merging");
    assert_eq!(by_bead("hq-bead-c").state, "merged");

    tokio::time::sleep(Duration::from_millis(10)).await;
    root.shutdown();
}

#[tokio::test]
async fn api_merges_empty_board_returns_empty_array() {
    let (base, root) = boot(vec![]).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base}/api/merges"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<gt_web::dto::MergeSlotDto> = resp.json().await.unwrap();
    assert!(body.is_empty());

    tokio::time::sleep(Duration::from_millis(10)).await;
    root.shutdown();
}
