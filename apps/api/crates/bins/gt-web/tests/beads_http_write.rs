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
use gt_beads::InMemoryBeads;
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
        beads: beads.clone(),
        sessions,
        agent_events: root.agent_events.clone(),
        events: root.events_sender(),
        town_root: None,
        issues: None,
        bus: Some(root.commands()),
        worktrees_stream: None,
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
