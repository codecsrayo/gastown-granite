//! Paso 8.2 gate (hq-8iur.2): the Rust sessions write-path owns the read-side truth.
//!
//! Drives a `Spawned → Spawned → SessionEnd` sequence through the running root's agent
//! relay. The sessions projector (broadcast → `SessionWriter`) mirrors each transition into
//! the sessions store, and `GET /api/sessions` reflects it live. Then the process is
//! "restarted" (root shut down, a fresh root spawned via `load_state`/`spawn_hydrated`
//! against the same log, with the same durable sessions store): `/api/sessions` still serves
//! the surviving active session, and the hydrated agent actor (8.1) knows both sessions from
//! the log — proving the write-path + boot-hydration cooperate.

use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;

use gt_agent::{AgentEvent, InMemorySessions, SessionRole};
use gt_beads::InMemoryBeads;
use gt_events::Envelope;
use gt_root::{
    load_state, root::Effects, spawn, spawn_hydrated, RootConfig, RootHandle, SystemClock,
};
use gt_web::{router, AppState, AuthConfig, InMemoryWebAudit, ReadinessGate, WebAuditSink};

struct NoopEffects;
impl Effects for NoopEffects {
    fn sling(&self, _convoy: &str, _member: &str) {}
    fn rotate(&self, _account: &str) {}
}

type Beads = Arc<InMemoryBeads>;

/// Bring up a root + sessions projector + HTTP server sharing one sessions store. When
/// `hydrate` is true the root is rebuilt from the log (the "after restart" leg).
async fn boot(
    sessions: Arc<InMemorySessions>,
    beads: Beads,
    log: std::path::PathBuf,
    hydrate: bool,
) -> (String, RootHandle<Beads>, tokio::task::JoinHandle<()>) {
    let merges = Arc::new(gt_merge::InMemoryMergeRepo::default());
    let root = if hydrate {
        let h = load_state(&log).expect("load_state");
        spawn_hydrated(
            beads.clone(),
            merges.clone(),
            Arc::new(gt_patrol::InMemoryPatrolRepo::default()),
            Arc::new(gt_orchestration::InMemoryOrchRepo::default()),
            NoopEffects,
            SystemClock,
            log,
            RootConfig::default(),
            h,
        )
    } else {
        spawn(
            beads.clone(),
            merges.clone(),
            Arc::new(gt_patrol::InMemoryPatrolRepo::default()),
            Arc::new(gt_orchestration::InMemoryOrchRepo::default()),
            NoopEffects,
            SystemClock,
            log,
            RootConfig::default(),
        )
    };
    let projector = gt_root::spawn_sessions_projector(&root, sessions.clone());
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
    (format!("http://{addr}"), root, projector)
}

async fn get_sessions(base: &str) -> Vec<gt_web::dto::SessionDto> {
    reqwest::Client::new()
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

/// Poll `/api/sessions` until `pred` holds or time out.
async fn wait_sessions(
    base: &str,
    pred: impl Fn(&[gt_web::dto::SessionDto]) -> bool,
) -> Vec<gt_web::dto::SessionDto> {
    for _ in 0..100 {
        let s = get_sessions(base).await;
        if pred(&s) {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timeout waiting for /api/sessions condition; last: {:?}", get_sessions(base).await);
}

#[tokio::test]
async fn sessions_writepath_reflects_lifecycle_live_and_after_restart() {
    let sessions = Arc::new(InMemorySessions::default());
    let beads: Beads = Arc::new(InMemoryBeads::default());
    let log = std::env::temp_dir().join(format!("gt-web-wp-{}.jsonl", ulid::Ulid::new()));
    let _ = std::fs::remove_file(&log);

    // ---- live: drive Spawned x2 then end one ----------------------------------------
    let (base1, root1, proj1) = boot(sessions.clone(), beads.clone(), log.clone(), false).await;

    let send = |ev: AgentEvent| {
        let relay = root1.agent_events.clone();
        async move { relay.send(Envelope::root(ev)).await.unwrap() }
    };
    send(AgentEvent::Spawned {
        session: "p1".into(),
        rig: "granite".into(),
        role: SessionRole::Polecat,
        crew: Some("atom".into()),
    })
    .await;
    send(AgentEvent::Spawned {
        session: "p2".into(),
        rig: "granite".into(),
        role: SessionRole::Polecat,
        crew: Some("brick".into()),
    })
    .await;

    // Both visible while active.
    wait_sessions(&base1, |s| s.len() == 2).await;

    // End p2 → only p1 stays active.
    send(AgentEvent::SessionEnd { session: "p2".into() }).await;
    let live = wait_sessions(&base1, |s| s.len() == 1).await;
    assert_eq!(live[0].id, "p1");
    assert_eq!(live[0].role, "polecat");
    assert_eq!(live[0].crew.as_deref(), Some("atom"));

    // ---- restart: new root hydrated from the log, same durable sessions store --------
    proj1.abort();
    root1.shutdown();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let (base2, root2, _proj2) = boot(sessions.clone(), beads.clone(), log.clone(), true).await;

    // The read-side still serves exactly the surviving active session — durable across the
    // restart (the sessions store was written by the write-path, not re-derived ad hoc).
    let after = get_sessions(&base2).await;
    assert_eq!(after.len(), 1, "one active session survives restart");
    assert_eq!(after[0].id, "p1");
    assert_eq!(after[0].crew.as_deref(), Some("atom"));

    // 8.1 cooperation: the hydrated agent actor knows BOTH sessions from the log (p1 active,
    // p2 done) — even though nobody called `agent.add` this run.
    let mut reg: Vec<_> = root2
        .agent
        .snapshot()
        .await
        .into_iter()
        .map(|s| (s.id, format!("{:?}", s.state)))
        .collect();
    reg.sort();
    assert_eq!(
        reg,
        vec![
            ("p1".to_string(), "Spawned".to_string()),
            ("p2".to_string(), "Done".to_string()),
        ],
        "boot hydration restored the session registry from the log",
    );

    root2.shutdown();
    let _ = std::fs::remove_file(&log);
}
