//! Gate for hq-fe-api-w.3: `POST /api/beads` and `PATCH /api/beads/:id` over HTTP.
//!
//! POST is the thin wrapper around `scheduling.create_bead`; PATCH partially updates
//! title/priority/assignee through the existing `BeadRepository` port (no new domain
//! command — the bead description's planned `bead.update` reduces to a read-modify-upsert
//! against the repo).

use std::sync::Arc;

use serde_json::json;
use tokio::net::TcpListener;

use gt_agent::InMemorySessions;
use gt_beads::{BeadRepository, InMemoryBeads};
use gt_root::{root::Effects, spawn, RootConfig, SystemClock};
use gt_web::{router, AppState, AuthConfig, InMemoryWebAudit, ReadinessGate, WebAuditSink};

struct NoopEffects;
impl Effects for NoopEffects {
    fn sling(&self, _convoy: &str, _member: &str) {}
    fn rotate(&self, _account: &str) {}
}

async fn boot() -> (
    String,
    Arc<InMemoryBeads>,
    gt_root::RootHandle<Arc<InMemoryBeads>>,
    tokio::task::JoinHandle<()>,
) {
    let beads = Arc::new(InMemoryBeads::default());
    let sessions = Arc::new(InMemorySessions::default());
    let log = {
        let mut p = std::env::temp_dir();
        p.push(format!("gt-web-beadhttp-{}.jsonl", ulid::Ulid::new()));
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
        beads: beads.clone(),
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
    };
    let sink: Arc<dyn WebAuditSink> = Arc::new(InMemoryWebAudit::new());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state, AuthConfig::open(), sink, ReadinessGate::ready());
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), beads, root, server)
}

#[tokio::test]
async fn post_creates_pending_bead_and_returns_201() {
    let (base, _beads, root, _srv) = boot().await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/beads"))
        .json(&json!({ "id": "hq-bw-1", "title": "test bead", "priority": 1 }))
        .send()
        .await
        .expect("send post");
    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["id"], "hq-bw-1");
    assert_eq!(body["status"], "pending");
    assert_eq!(body["priority"], 1);
    root.shutdown();
}

#[tokio::test]
async fn post_rejects_invalid_payload() {
    let (base, _beads, root, _srv) = boot().await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/beads"))
        .json(&json!({ "id": "", "title": "x" }))
        .send()
        .await
        .expect("send post");
    assert_eq!(resp.status(), 400);

    let resp = reqwest::Client::new()
        .post(format!("{base}/api/beads"))
        .json(&json!({ "id": "hq-x", "title": "x", "priority": 9 }))
        .send()
        .await
        .expect("send post");
    assert_eq!(resp.status(), 400);
    root.shutdown();
}

#[tokio::test]
async fn patch_updates_editable_fields_only() {
    let (base, _beads, root, _srv) = boot().await;
    reqwest::Client::new()
        .post(format!("{base}/api/beads"))
        .json(&json!({ "id": "hq-bw-2", "title": "before", "priority": 2 }))
        .send()
        .await
        .expect("seed");

    let resp = reqwest::Client::new()
        .patch(format!("{base}/api/beads/hq-bw-2"))
        .json(&json!({ "title": "after", "priority": 0, "assignee": "alice" }))
        .send()
        .await
        .expect("patch");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["title"], "after");
    assert_eq!(body["priority"], 0);
    assert_eq!(body["assignee"], "alice");
    assert_eq!(body["status"], "pending", "PATCH must not move status");

    root.shutdown();
}

#[tokio::test]
async fn patch_empty_body_rejects_400() {
    let (base, _beads, root, _srv) = boot().await;
    reqwest::Client::new()
        .post(format!("{base}/api/beads"))
        .json(&json!({ "id": "hq-bw-3", "title": "x", "priority": 2 }))
        .send()
        .await
        .expect("seed");

    let resp = reqwest::Client::new()
        .patch(format!("{base}/api/beads/hq-bw-3"))
        .json(&json!({}))
        .send()
        .await
        .expect("patch");
    assert_eq!(resp.status(), 400);
    root.shutdown();
}

#[tokio::test]
async fn patch_missing_id_returns_404() {
    let (base, _beads, root, _srv) = boot().await;
    let resp = reqwest::Client::new()
        .patch(format!("{base}/api/beads/hq-no-such"))
        .json(&json!({ "title": "x" }))
        .send()
        .await
        .expect("patch");
    assert_eq!(resp.status(), 404);
    root.shutdown();
}

// hq-fe-api-w.4: POST /api/beads/:id/transition — operator state-machine override.
// The route does not touch dispatcher capacity (the gate verifies status only); a real
// `MergeEvent::Merged` flow stays the canonical close path.

async fn seed(base: &str, id: &str) {
    reqwest::Client::new()
        .post(format!("{base}/api/beads"))
        .json(&json!({ "id": id, "title": "seed", "priority": 2 }))
        .send()
        .await
        .expect("seed");
}

#[tokio::test]
async fn transition_pending_to_done_succeeds() {
    let (base, beads, root, _srv) = boot().await;
    seed(&base, "hq-tx-1").await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/beads/hq-tx-1/transition"))
        .json(&json!({ "to": "done" }))
        .send()
        .await
        .expect("transition");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["status"], "done");
    let stored = beads.get("hq-tx-1").await.unwrap().expect("bead");
    assert_eq!(stored.status, gt_beads::BeadStatus::Done);
    root.shutdown();
}

#[tokio::test]
async fn transition_pending_to_dispatched_rejected() {
    // Scheduler-owned move: `pending → dispatched` must stay on `scheduling.mark_dispatched`
    // so capacity bookkeeping is not bypassed.
    let (base, _beads, root, _srv) = boot().await;
    seed(&base, "hq-tx-2").await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/beads/hq-tx-2/transition"))
        .json(&json!({ "to": "dispatched" }))
        .send()
        .await
        .expect("transition");
    assert_eq!(resp.status(), 400);
    root.shutdown();
}

#[tokio::test]
async fn transition_done_to_failed_rejected() {
    // Terminal-to-terminal crossover must round-trip through `pending` so the re-open is
    // explicit. Seed via repo to bypass the create handler's `Pending`-only contract.
    let (base, beads, root, _srv) = boot().await;
    beads
        .upsert(&gt_beads::Bead::new(
            "hq-tx-3",
            "done bead",
            gt_beads::BeadStatus::Done,
            2,
        ))
        .await
        .unwrap();
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/beads/hq-tx-3/transition"))
        .json(&json!({ "to": "failed" }))
        .send()
        .await
        .expect("transition");
    assert_eq!(resp.status(), 400);
    root.shutdown();
}

#[tokio::test]
async fn transition_self_loop_rejected() {
    let (base, _beads, root, _srv) = boot().await;
    seed(&base, "hq-tx-4").await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/beads/hq-tx-4/transition"))
        .json(&json!({ "to": "pending" }))
        .send()
        .await
        .expect("transition");
    assert_eq!(resp.status(), 400);
    root.shutdown();
}

#[tokio::test]
async fn transition_unknown_target_rejected() {
    let (base, _beads, root, _srv) = boot().await;
    seed(&base, "hq-tx-5").await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/beads/hq-tx-5/transition"))
        .json(&json!({ "to": "winning" }))
        .send()
        .await
        .expect("transition");
    assert_eq!(resp.status(), 400);
    root.shutdown();
}

#[tokio::test]
async fn transition_missing_bead_returns_404() {
    let (base, _beads, root, _srv) = boot().await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/beads/hq-no-such/transition"))
        .json(&json!({ "to": "done" }))
        .send()
        .await
        .expect("transition");
    assert_eq!(resp.status(), 404);
    root.shutdown();
}

#[tokio::test]
async fn transition_done_to_pending_reopens() {
    // Re-open path: a terminal row goes back to `pending` so the scheduler can re-pick it.
    let (base, beads, root, _srv) = boot().await;
    beads
        .upsert(&gt_beads::Bead::new(
            "hq-tx-6",
            "closed bead",
            gt_beads::BeadStatus::Done,
            2,
        ))
        .await
        .unwrap();
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/beads/hq-tx-6/transition"))
        .json(&json!({ "to": "pending" }))
        .send()
        .await
        .expect("transition");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["status"], "pending");
    root.shutdown();
}

#[tokio::test]
async fn patch_clears_assignee_with_empty_string() {
    let (base, _beads, root, _srv) = boot().await;
    reqwest::Client::new()
        .post(format!("{base}/api/beads"))
        .json(&json!({
            "id": "hq-bw-4",
            "title": "x",
            "priority": 2,
            "assignee": "alice",
        }))
        .send()
        .await
        .expect("seed");

    let resp = reqwest::Client::new()
        .patch(format!("{base}/api/beads/hq-bw-4"))
        .json(&json!({ "assignee": "" }))
        .send()
        .await
        .expect("patch");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert!(body["assignee"].is_null(), "empty string clears assignee");
    root.shutdown();
}
