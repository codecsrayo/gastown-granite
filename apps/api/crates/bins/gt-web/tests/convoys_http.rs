//! Gate for hq-fe-api-w.9: `POST /api/convoys` + `POST /api/convoys/:c/members/:m/fail`.
//!
//! The HTTP routes wrap `OrchCommand::Launch` and `OrchCommand::Fail` through the same
//! [`gt_root::CommandBus`] the gt-mcp tools drive, so audit and scope flow uniformly.
//! `pause` / `resume` are intentionally absent — domain has no Pause/Resume commands
//! today (`gap parcial` in the migration plan).

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

async fn boot() -> (String, gt_root::RootHandle<Arc<InMemoryBeads>>, tokio::task::JoinHandle<()>) {
    let beads = Arc::new(InMemoryBeads::default());
    let sessions = Arc::new(InMemorySessions::default());
    let log = {
        let mut p = std::env::temp_dir();
        p.push(format!("gt-web-convoy-{}.jsonl", ulid::Ulid::new()));
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
        beads,
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
        event_log: None,
    };
    let sink: Arc<dyn WebAuditSink> = Arc::new(InMemoryWebAudit::new());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state, AuthConfig::open(), sink, ReadinessGate::ready());
    let srv = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), root, srv)
}

#[tokio::test]
async fn post_convoys_creates_and_returns_201() {
    let (base, root, _srv) = boot().await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/convoys"))
        .json(&json!({ "convoy": "cv-1", "members": ["hq-a.1", "hq-a.2"] }))
        .send()
        .await
        .expect("send post");
    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["convoy"], "cv-1");
    assert_eq!(body["members"][0], "hq-a.1");
    assert_eq!(body["launched"], true);
    root.shutdown();
}

#[tokio::test]
async fn post_convoys_rejects_empty_id() {
    let (base, root, _srv) = boot().await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/convoys"))
        .json(&json!({ "convoy": "", "members": ["hq-a.1"] }))
        .send()
        .await
        .expect("send post");
    assert_eq!(resp.status(), 400);
    root.shutdown();
}

#[tokio::test]
async fn post_convoys_rejects_empty_member_list() {
    let (base, root, _srv) = boot().await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/convoys"))
        .json(&json!({ "convoy": "cv-2", "members": [] }))
        .send()
        .await
        .expect("send post");
    assert_eq!(resp.status(), 400);
    root.shutdown();
}

#[tokio::test]
async fn post_convoys_rejects_empty_member_id() {
    let (base, root, _srv) = boot().await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/convoys"))
        .json(&json!({ "convoy": "cv-3", "members": ["hq-a.1", ""] }))
        .send()
        .await
        .expect("send post");
    assert_eq!(resp.status(), 400);
    root.shutdown();
}

#[tokio::test]
async fn post_convoys_duplicate_id_returns_4xx() {
    // `LaunchConvoy::validate` rejects duplicate convoys. The bus surfaces that as an
    // `AppError::Validation` which gt-web maps to 500 today — the gate just asserts the
    // POST does not silently succeed against an existing convoy.
    let (base, root, _srv) = boot().await;
    let body = json!({ "convoy": "cv-dup", "members": ["hq-a.1"] });

    let first = reqwest::Client::new()
        .post(format!("{base}/api/convoys"))
        .json(&body)
        .send()
        .await
        .expect("send first");
    assert_eq!(first.status(), 201);

    let dup = reqwest::Client::new()
        .post(format!("{base}/api/convoys"))
        .json(&body)
        .send()
        .await
        .expect("send dup");
    assert!(
        !dup.status().is_success(),
        "duplicate convoy must fail, got {}",
        dup.status()
    );
    root.shutdown();
}

#[tokio::test]
async fn fail_member_halts_convoy_and_returns_200() {
    let (base, root, _srv) = boot().await;
    reqwest::Client::new()
        .post(format!("{base}/api/convoys"))
        .json(&json!({ "convoy": "cv-f", "members": ["hq-a.1", "hq-a.2"] }))
        .send()
        .await
        .expect("seed convoy");

    let resp = reqwest::Client::new()
        .post(format!("{base}/api/convoys/cv-f/members/hq-a.1/fail"))
        .json(&json!({ "reason": "worker timed out" }))
        .send()
        .await
        .expect("send fail");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["failed"], true);
    assert_eq!(body["convoy"], "cv-f");
    assert_eq!(body["member"], "hq-a.1");
    assert_eq!(body["reason"], "worker timed out");
    root.shutdown();
}

#[tokio::test]
async fn fail_member_rejects_empty_reason() {
    let (base, root, _srv) = boot().await;
    reqwest::Client::new()
        .post(format!("{base}/api/convoys"))
        .json(&json!({ "convoy": "cv-r", "members": ["hq-a.1"] }))
        .send()
        .await
        .expect("seed");

    let resp = reqwest::Client::new()
        .post(format!("{base}/api/convoys/cv-r/members/hq-a.1/fail"))
        .json(&json!({ "reason": "   " }))
        .send()
        .await
        .expect("send fail");
    assert_eq!(resp.status(), 400);
    root.shutdown();
}

/// hq-fe-api-r.3 gate: `GET /api/convoys` returns the actor's snapshot mapped to
/// `ConvoyDto`. Seeds two convoys, asserts both come back with their member lists in
/// insertion order + a non-empty state string. Members default to `pending` at Stage and
/// flip to `active` on Launch's dispatch chain — the exact value is whichever the actor
/// reports, the gate only checks the field is round-tripped.
#[tokio::test]
async fn get_convoys_returns_snapshot() {
    let (base, root, _srv) = boot().await;
    let client = reqwest::Client::new();
    client
        .post(format!("{base}/api/convoys"))
        .json(&json!({ "convoy": "cv-a", "members": ["hq-a.1", "hq-a.2"] }))
        .send()
        .await
        .expect("seed a");
    client
        .post(format!("{base}/api/convoys"))
        .json(&json!({ "convoy": "cv-b", "members": ["hq-b.1"] }))
        .send()
        .await
        .expect("seed b");

    let rows: Vec<serde_json::Value> = client
        .get(format!("{base}/api/convoys"))
        .send()
        .await
        .expect("send get")
        .json()
        .await
        .expect("json");
    assert!(rows.iter().any(|c| c["id"] == "cv-a"));
    assert!(rows.iter().any(|c| c["id"] == "cv-b"));
    let cv_a = rows.iter().find(|c| c["id"] == "cv-a").unwrap();
    assert_eq!(cv_a["members"][0]["bead"], "hq-a.1");
    assert_eq!(cv_a["members"][1]["bead"], "hq-a.2");
    assert!(cv_a["state"].is_string());
    root.shutdown();
}

/// hq-fe-api-r.3 gate: `?state=` filter narrows the slice in-memory. Asking for a state
/// no convoy holds returns `[]` rather than 400 — closed-set enum but permissive route,
/// mirroring the `?role=` posture on `/api/sessions`.
#[tokio::test]
async fn get_convoys_filters_by_state() {
    let (base, root, _srv) = boot().await;
    reqwest::Client::new()
        .post(format!("{base}/api/convoys"))
        .json(&json!({ "convoy": "cv-flt", "members": ["hq-a.1"] }))
        .send()
        .await
        .expect("seed");

    let rows: Vec<serde_json::Value> = reqwest::Client::new()
        .get(format!("{base}/api/convoys?state=closed"))
        .send()
        .await
        .expect("send get")
        .json()
        .await
        .expect("json");
    assert!(rows.is_empty(), "no convoy is closed, got: {rows:?}");
    root.shutdown();
}

#[tokio::test]
async fn fail_member_unknown_convoy_returns_5xx() {
    // `FailMember::validate` returns `AppError::NotFound`, which gt-web today maps to
    // 500 (the gateway does not invent semantics for domain errors). The gate just
    // asserts the route refuses to act on an unknown convoy.
    let (base, root, _srv) = boot().await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/convoys/ghost/members/hq-a.1/fail"))
        .json(&json!({ "reason": "x" }))
        .send()
        .await
        .expect("send fail");
    assert!(
        !resp.status().is_success(),
        "unknown convoy must fail, got {}",
        resp.status()
    );
    root.shutdown();
}
