//! Gate for hq-fe-api-w.6: `DELETE /api/sessions/:id`. The route is the operator's
//! e-stop on a runaway polecat: it (a) confirms the session is still active, (b) calls
//! the `PolecatControl` port (production: tmux kill-session), and (c) emits
//! `AgentEvent::Killed` on the agent relay so the projector flips the row and SSE
//! subscribers see the lifecycle close.
//!
//! Setup mirrors the other `gt-web` integration tests: a real `RootHandle` + in-memory
//! ports + a recording [`InMemoryPolecatControl`] so each gate can assert the route
//! reached the edge with the right session id.

use std::sync::Arc;

use tokio::net::TcpListener;

use gt_agent::{AgentEvent, InMemorySessions, Session, SessionWriter};
use gt_beads::InMemoryBeads;
use gt_events::Envelope;
use gt_root::{root::Effects, spawn, RootConfig, SystemClock};
use gt_web::{
    router, AppState, AuthConfig, InMemoryPolecatControl, InMemoryWebAudit, PolecatControl,
    ReadinessGate, WebAuditSink,
};

struct NoopEffects;
impl Effects for NoopEffects {
    fn sling(&self, _convoy: &str, _member: &str) {}
    fn rotate(&self, _account: &str) {}
}

struct Setup {
    base: String,
    sessions: Arc<InMemorySessions>,
    killer: Arc<InMemoryPolecatControl>,
    root: gt_root::RootHandle<Arc<InMemoryBeads>>,
    agent_tx: tokio::sync::mpsc::Sender<Envelope<AgentEvent>>,
    _srv: tokio::task::JoinHandle<()>,
}

async fn boot(with_killer: bool) -> Setup {
    let beads = Arc::new(InMemoryBeads::default());
    let sessions = Arc::new(InMemorySessions::default());
    let log = {
        let mut p = std::env::temp_dir();
        p.push(format!("gt-web-sesskill-{}.jsonl", ulid::Ulid::new()));
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
    let killer = Arc::new(InMemoryPolecatControl::new());
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
        control: if with_killer {
            Some(killer.clone() as Arc<dyn PolecatControl>)
        } else {
            None
        },
        respawner: None,
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
        killer,
        root,
        agent_tx,
        _srv: srv,
    }
}

async fn seed_session(sessions: &InMemorySessions, id: &str) {
    sessions
        .upsert(&Session::new(id, "granite"))
        .await
        .expect("seed session");
}

#[tokio::test]
async fn delete_unknown_session_returns_404() {
    let s = boot(true).await;
    let resp = reqwest::Client::new()
        .delete(format!("{}/api/sessions/ghost", s.base))
        .send()
        .await
        .expect("send delete");
    assert_eq!(resp.status(), 404);
    assert!(s.killer.killed().is_empty(), "no tmux kill on 404");
    s.root.shutdown();
}

#[tokio::test]
async fn delete_active_session_calls_killer_and_returns_204() {
    let s = boot(true).await;
    seed_session(&s.sessions, "gt-furiosa-hq-bw-9").await;

    let resp = reqwest::Client::new()
        .delete(format!("{}/api/sessions/gt-furiosa-hq-bw-9", s.base))
        .send()
        .await
        .expect("send delete");
    assert_eq!(resp.status(), 204);
    assert_eq!(
        s.killer.killed(),
        vec!["gt-furiosa-hq-bw-9".to_string()],
        "tmux kill called with the canonical session id (not a re-derived name)",
    );
    s.root.shutdown();
}

#[tokio::test]
async fn delete_emits_agent_killed_for_projector() {
    // The route must publish `AgentEvent::Killed` on the relay so the sessions projector
    // flips the row to `Killed` and SSE subscribers see `agent.killed`. We watch the
    // root's broadcast tap and assert the kind+reason after the request returns.
    let s = boot(true).await;
    let mut rx = s.root.events_sender().subscribe();
    seed_session(&s.sessions, "gt-furiosa-hq-bw-7").await;

    let resp = reqwest::Client::new()
        .delete(format!("{}/api/sessions/gt-furiosa-hq-bw-7", s.base))
        .send()
        .await
        .expect("send delete");
    assert_eq!(resp.status(), 204);

    // Drain until we see the agent.killed record (other audit records may interleave).
    let killed = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let rec = rx.recv().await.expect("recv event");
            if rec.kind == "agent.killed" {
                break rec;
            }
        }
    })
    .await
    .expect("agent.killed within 2s");
    let payload_str = killed.payload.to_string();
    assert!(
        payload_str.contains("gt-furiosa-hq-bw-7"),
        "session id appears in the killed event payload: {payload_str}",
    );
    // Drop the unused sender clone so the connection task winds down cleanly.
    drop(s.agent_tx);
    s.root.shutdown();
}

#[tokio::test]
async fn delete_with_no_killer_returns_500() {
    // A bin that never wires a killer must surface the misconfiguration to the operator
    // (not pretend the kill succeeded). The handler returns 500 with a wire-readable
    // error message; the session row is left intact.
    let s = boot(false).await;
    seed_session(&s.sessions, "gt-furiosa-hq-bw-5").await;

    let resp = reqwest::Client::new()
        .delete(format!("{}/api/sessions/gt-furiosa-hq-bw-5", s.base))
        .send()
        .await
        .expect("send delete");
    assert_eq!(resp.status(), 500);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert!(
        body["error"].as_str().unwrap_or_default().contains("control"),
        "error mentions control wiring: {body}",
    );
    s.root.shutdown();
}

// hq-fe-api-w.8 — `POST /api/sessions/:id/interrupt`. Softer e-stop: sends `Escape`
// via tmux `send-keys`, cancelling the agent's in-flight turn without killing the
// polecat. Same registry pre-check as DELETE; no `AgentEvent` emit (the lifecycle row
// stays in its current state — the polecat is still alive).

#[tokio::test]
async fn interrupt_unknown_session_returns_404() {
    let s = boot(true).await;
    let resp = reqwest::Client::new()
        .post(format!("{}/api/sessions/ghost/interrupt", s.base))
        .send()
        .await
        .expect("send interrupt");
    assert_eq!(resp.status(), 404);
    assert!(
        s.killer.keys_sent().is_empty(),
        "no send-keys on 404"
    );
    s.root.shutdown();
}

#[tokio::test]
async fn interrupt_active_session_sends_escape_and_returns_204() {
    let s = boot(true).await;
    seed_session(&s.sessions, "gt-furiosa-hq-iq-9").await;

    let resp = reqwest::Client::new()
        .post(format!("{}/api/sessions/gt-furiosa-hq-iq-9/interrupt", s.base))
        .send()
        .await
        .expect("send interrupt");
    assert_eq!(resp.status(), 204);
    assert_eq!(
        s.killer.keys_sent(),
        vec![("gt-furiosa-hq-iq-9".to_string(), vec!["Escape".to_string()])],
        "send-keys called with canonical session id + Escape chord",
    );
    assert!(
        s.killer.killed().is_empty(),
        "interrupt does not kill the session — that is DELETE"
    );
    s.root.shutdown();
}

#[tokio::test]
async fn interrupt_without_control_returns_500() {
    let s = boot(false).await;
    seed_session(&s.sessions, "gt-furiosa-hq-iq-3").await;

    let resp = reqwest::Client::new()
        .post(format!("{}/api/sessions/gt-furiosa-hq-iq-3/interrupt", s.base))
        .send()
        .await
        .expect("send interrupt");
    assert_eq!(resp.status(), 500);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert!(
        body["error"].as_str().unwrap_or_default().contains("control"),
        "error mentions control wiring: {body}",
    );
    s.root.shutdown();
}

#[tokio::test]
async fn interrupt_does_not_emit_agent_killed() {
    // The polecat keeps running, so no `agent.killed` record should land on the audit
    // broadcast as a side effect of an interrupt. We watch the tap for ~500ms after the
    // request returns and assert no killed event appeared.
    let s = boot(true).await;
    let mut rx = s.root.events_sender().subscribe();
    seed_session(&s.sessions, "gt-furiosa-hq-iq-5").await;

    let resp = reqwest::Client::new()
        .post(format!("{}/api/sessions/gt-furiosa-hq-iq-5/interrupt", s.base))
        .send()
        .await
        .expect("send interrupt");
    assert_eq!(resp.status(), 204);

    let saw_killed = tokio::time::timeout(std::time::Duration::from_millis(500), async {
        loop {
            match rx.recv().await {
                Ok(rec) if rec.kind == "agent.killed" => break true,
                Ok(_) => continue,
                Err(_) => break false,
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(!saw_killed, "interrupt must not produce agent.killed");
    drop(s.agent_tx);
    s.root.shutdown();
}

#[tokio::test]
async fn delete_empty_id_rejected() {
    // The axum route binds `:id` to a non-empty path segment, so an empty id practically
    // can't reach the handler — the router 404s first. The explicit empty-check on the
    // handler is the belt + suspender for in-process tests that bypass the router; this
    // gate exercises the public surface and just asserts the 4xx posture.
    let s = boot(true).await;
    let resp = reqwest::Client::new()
        .delete(format!("{}/api/sessions/", s.base))
        .send()
        .await
        .expect("send delete");
    assert!(
        resp.status().is_client_error(),
        "empty id surfaces as 4xx, got {}",
        resp.status()
    );
    s.root.shutdown();
}
