//! Gate for hq-fe-api-w.7: `POST /api/sessions/:id/restart`. Cold-restart a stuck
//! polecat: reads the dying session's `GT_HOOK_BEAD` + `GT_CONVOY`, tears the tmux
//! session down, and slings a fresh polecat with the same hook. Emits the close+reopen
//! `AgentEvent` pair (`Killed` + `Spawned`) the supervisor would emit on a real restart
//! so SSE subscribers + projector see a single atomic transition.

use std::sync::Arc;

use tokio::net::TcpListener;

use gt_agent::{InMemorySessions, Session, SessionWriter};
use gt_beads::InMemoryBeads;
use gt_root::{root::Effects, spawn, RootConfig, SystemClock};
use gt_web::{
    router, AppState, AuthConfig, InMemoryPolecatRespawner, InMemoryWebAudit, PolecatRespawner,
    ReadinessGate, RespawnInfo, WebAuditSink,
};

struct NoopEffects;
impl Effects for NoopEffects {
    fn sling(&self, _convoy: &str, _member: &str) {}
    fn rotate(&self, _account: &str) {}
}

struct Setup {
    base: String,
    sessions: Arc<InMemorySessions>,
    respawner: Arc<InMemoryPolecatRespawner>,
    root: gt_root::RootHandle<Arc<InMemoryBeads>>,
    agent_tx: tokio::sync::mpsc::Sender<gt_events::Envelope<gt_agent::AgentEvent>>,
    _srv: tokio::task::JoinHandle<()>,
}

async fn boot(canned: RespawnInfo, with_respawner: bool) -> Setup {
    let beads = Arc::new(InMemoryBeads::default());
    let sessions = Arc::new(InMemorySessions::default());
    let log = {
        let mut p = std::env::temp_dir();
        p.push(format!("gt-web-restart-{}.jsonl", ulid::Ulid::new()));
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
    let respawner = Arc::new(InMemoryPolecatRespawner::new(canned));
    let agent_tx = root.agent_events.clone();
    let state = AppState {
        beads: beads.clone(),
        sessions: sessions.clone(),
        merges: merges.clone(),
        agent_events: agent_tx.clone(),
        events: root.events_sender(),
        town_root: None,
        issues: None,
        bus: Some(root.commands()),
        worktrees_stream: None,
        control: None,
        respawner: if with_respawner {
            Some(respawner.clone() as Arc<dyn PolecatRespawner>)
        } else {
            None
        },
        commenter: None,
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
        sessions,
        respawner,
        root,
        agent_tx,
        _srv: srv,
    }
}

async fn seed(sessions: &InMemorySessions, id: &str) {
    sessions
        .upsert(&Session::new(id, "granite"))
        .await
        .expect("seed");
}

fn canned(session: &str, member: &str, convoy: Option<&str>) -> RespawnInfo {
    RespawnInfo {
        session: session.to_string(),
        rig: "granite".to_string(),
        member: member.to_string(),
        convoy: convoy.map(str::to_string),
    }
}

#[tokio::test]
async fn restart_unknown_session_returns_404() {
    let s = boot(canned("x", "x", None), true).await;
    let resp = reqwest::Client::new()
        .post(format!("{}/api/sessions/ghost/restart", s.base))
        .send()
        .await
        .expect("send restart");
    assert_eq!(resp.status(), 404);
    assert!(s.respawner.restarts().is_empty(), "no respawn on 404");
    s.root.shutdown();
}

#[tokio::test]
async fn restart_active_session_returns_200_with_info() {
    let s = boot(
        canned("gt-furiosa-hq-rs-1", "hq-rs-1", Some("cv-1")),
        true,
    )
    .await;
    seed(&s.sessions, "gt-furiosa-hq-rs-1").await;

    let resp = reqwest::Client::new()
        .post(format!("{}/api/sessions/gt-furiosa-hq-rs-1/restart", s.base))
        .send()
        .await
        .expect("send restart");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["restarted"], true);
    assert_eq!(body["session"], "gt-furiosa-hq-rs-1");
    assert_eq!(body["rig"], "granite");
    assert_eq!(body["member"], "hq-rs-1");
    assert_eq!(body["convoy"], "cv-1");
    assert_eq!(
        s.respawner.restarts(),
        vec!["gt-furiosa-hq-rs-1".to_string()],
        "respawn called with canonical session id",
    );
    s.root.shutdown();
}

#[tokio::test]
async fn restart_emits_killed_and_spawned_envelopes() {
    // The handler must publish both `agent.killed` and `agent.spawned` on the relay so
    // the projector flips the dying row to `Killed` and inserts the fresh polecat row.
    // We watch the broadcast for both kinds within a generous timeout.
    let s = boot(
        canned("gt-furiosa-hq-rs-2", "hq-rs-2", None),
        true,
    )
    .await;
    let mut rx = s.root.events_sender().subscribe();
    seed(&s.sessions, "gt-furiosa-hq-rs-2").await;

    let resp = reqwest::Client::new()
        .post(format!("{}/api/sessions/gt-furiosa-hq-rs-2/restart", s.base))
        .send()
        .await
        .expect("send restart");
    assert_eq!(resp.status(), 200);

    let pair = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        let mut saw_killed = false;
        let mut saw_spawned = false;
        while !(saw_killed && saw_spawned) {
            match rx.recv().await {
                Ok(rec) if rec.kind == "agent.killed" => saw_killed = true,
                Ok(rec) if rec.kind == "agent.spawned" => saw_spawned = true,
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        (saw_killed, saw_spawned)
    })
    .await
    .expect("both events within 2s");
    assert_eq!(pair, (true, true));
    drop(s.agent_tx);
    s.root.shutdown();
}

#[tokio::test]
async fn restart_without_respawner_returns_500() {
    let s = boot(canned("x", "x", None), false).await;
    seed(&s.sessions, "gt-furiosa-hq-rs-3").await;

    let resp = reqwest::Client::new()
        .post(format!("{}/api/sessions/gt-furiosa-hq-rs-3/restart", s.base))
        .send()
        .await
        .expect("send restart");
    assert_eq!(resp.status(), 500);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert!(
        body["error"].as_str().unwrap_or_default().contains("respawner"),
        "error mentions respawner wiring: {body}",
    );
    s.root.shutdown();
}

#[tokio::test]
async fn restart_propagates_respawner_info_for_renamed_session() {
    // If the lifecycle yields a session id different from the request path (e.g. an
    // operator renamed the rig prefix mid-flight), the response carries the new id so
    // the dashboard can re-subscribe. The 200 body is the contract.
    let s = boot(
        canned("gt-asgard-hq-rs-4", "hq-rs-4", Some("cv-9")),
        true,
    )
    .await;
    seed(&s.sessions, "gt-furiosa-hq-rs-4").await;

    let resp = reqwest::Client::new()
        .post(format!("{}/api/sessions/gt-furiosa-hq-rs-4/restart", s.base))
        .send()
        .await
        .expect("send restart");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["session"], "gt-asgard-hq-rs-4");
    assert_eq!(body["convoy"], "cv-9");
    s.root.shutdown();
}
