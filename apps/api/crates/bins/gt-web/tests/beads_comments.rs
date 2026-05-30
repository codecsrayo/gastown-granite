//! Gate for hq-fe-api-w.5: `POST /api/beads/:id/comments`. Appends a free-text
//! operator comment to the issue's `notes` column. Storage shape is flat text
//! formatted as `\n[YYYY-MM-DDTHH:MM:SSZ] @author: body`; a future migration
//! to a structured `issue_comments` table can split on the same separators.

use std::sync::Arc;

use serde_json::json;
use tokio::net::TcpListener;

use gt_agent::InMemorySessions;
use gt_beads::InMemoryBeads;
use gt_root::{root::Effects, spawn, RootConfig, SystemClock};
use gt_web::{
    router, AppState, AuthConfig, InMemoryIssueCommenter, InMemoryWebAudit, IssueCommenter,
    ReadinessGate, WebAuditSink,
};

struct NoopEffects;
impl Effects for NoopEffects {
    fn sling(&self, _convoy: &str, _member: &str) {}
    fn rotate(&self, _account: &str) {}
}

struct Setup {
    base: String,
    commenter: Arc<InMemoryIssueCommenter>,
    root: gt_root::RootHandle<Arc<InMemoryBeads>>,
    _srv: tokio::task::JoinHandle<()>,
}

async fn boot(with_commenter: bool) -> Setup {
    let beads = Arc::new(InMemoryBeads::default());
    let sessions = Arc::new(InMemorySessions::default());
    let log = {
        let mut p = std::env::temp_dir();
        p.push(format!("gt-web-comment-{}.jsonl", ulid::Ulid::new()));
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
    let commenter = Arc::new(InMemoryIssueCommenter::new());
    let merges = Arc::new(gt_merge::InMemoryMergeRepo::default());
    let state = AppState {
        beads,
        sessions,
        merges,
        agent_events: root.agent_events.clone(),
        events: root.events_sender(),
        town_root: None,
        issues: None,
        bus: Some(root.commands()),
        worktrees_stream: None,
        control: None,
        respawner: None,
        commenter: if with_commenter {
            Some(commenter.clone() as Arc<dyn IssueCommenter>)
        } else {
            None
        },
        event_log: None,
        login_registry: std::sync::Arc::new(gt_web::LoginRegistry::new()),
        login_pty: None,
        login_config: std::sync::Arc::new(gt_web::LoginConfig::default()),
    };
    let sink: Arc<dyn WebAuditSink> = Arc::new(InMemoryWebAudit::new());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state, AuthConfig::open(), sink, ReadinessGate::ready());
    let srv = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Setup {
        base: format!("http://{addr}"),
        commenter,
        root,
        _srv: srv,
    }
}

#[tokio::test]
async fn post_comment_returns_201_with_formatted_fragment() {
    let s = boot(true).await;
    let resp = reqwest::Client::new()
        .post(format!("{}/api/beads/hq-c-1/comments", s.base))
        .json(&json!({ "body": "first note", "author": "alice" }))
        .send()
        .await
        .expect("send post");
    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["id"], "hq-c-1");
    let appended = body["appended"].as_str().expect("appended is str");
    assert!(appended.starts_with("\n["), "leading newline + timestamp: {appended}");
    assert!(appended.contains("@alice: first note"), "embeds author + body: {appended}");
    assert!(body["ts"].as_str().unwrap_or_default().ends_with('Z'),
        "ts is RFC3339 Z-suffix: {body}");

    let calls = s.commenter.appended();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "hq-c-1");
    assert_eq!(calls[0].1, appended);
    s.root.shutdown();
}

#[tokio::test]
async fn post_comment_defaults_anon_when_author_missing() {
    let s = boot(true).await;
    let resp = reqwest::Client::new()
        .post(format!("{}/api/beads/hq-c-2/comments", s.base))
        .json(&json!({ "body": "no author here" }))
        .send()
        .await
        .expect("send post");
    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert!(
        body["appended"].as_str().unwrap_or_default().contains("@anon: no author here"),
        "anon fallback when author missing: {body}",
    );
    s.root.shutdown();
}

#[tokio::test]
async fn post_comment_rejects_empty_body() {
    let s = boot(true).await;
    let resp = reqwest::Client::new()
        .post(format!("{}/api/beads/hq-c-3/comments", s.base))
        .json(&json!({ "body": "   " }))
        .send()
        .await
        .expect("send post");
    assert_eq!(resp.status(), 400);
    assert!(
        s.commenter.appended().is_empty(),
        "no edge call on validation failure"
    );
    s.root.shutdown();
}

#[tokio::test]
async fn post_comment_rejects_oversize_body() {
    let s = boot(true).await;
    let big = "x".repeat(4097);
    let resp = reqwest::Client::new()
        .post(format!("{}/api/beads/hq-c-4/comments", s.base))
        .json(&json!({ "body": big }))
        .send()
        .await
        .expect("send post");
    assert_eq!(resp.status(), 400);
    s.root.shutdown();
}

#[tokio::test]
async fn post_comment_returns_404_for_unknown_issue() {
    let s = boot(true).await;
    s.commenter.set_not_found("hq-ghost");
    let resp = reqwest::Client::new()
        .post(format!("{}/api/beads/hq-ghost/comments", s.base))
        .json(&json!({ "body": "hi" }))
        .send()
        .await
        .expect("send post");
    assert_eq!(resp.status(), 404);
    s.root.shutdown();
}

#[tokio::test]
async fn post_comment_returns_500_when_commenter_unwired() {
    let s = boot(false).await;
    let resp = reqwest::Client::new()
        .post(format!("{}/api/beads/hq-c-5/comments", s.base))
        .json(&json!({ "body": "x" }))
        .send()
        .await
        .expect("send post");
    assert_eq!(resp.status(), 500);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert!(
        body["error"].as_str().unwrap_or_default().contains("commenter"),
        "error mentions commenter wiring: {body}",
    );
    s.root.shutdown();
}
