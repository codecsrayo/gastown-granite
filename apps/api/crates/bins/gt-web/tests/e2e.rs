//! Gate test for Paso 6.f. The doc rule (`docs/07-frontend.md`) says the browser app sees
//! exactly two channels: a REST snapshot and an SSE stream that ships the same `EventRecord`
//! the audit log persists. This test wires the real router on a transient socket, hits both,
//! and asserts that a domain event driven through the running root reaches the SSE listener
//! as a JSON `EventRecord`.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use tokio::net::TcpListener;

use gt_agent::{AgentEvent, InMemorySessions, Session, SessionRole};
use gt_beads::{Bead, BeadRepository, BeadStatus, InMemoryBeads};
use gt_events::Envelope;
use gt_root::{root::Effects, spawn, RootConfig, SystemClock};
use gt_web::{
    router, AppState, AuthConfig, InMemoryWebAudit, ReadinessGate, WebAuditEvent, WebAuditSink,
};

struct NoopEffects;
impl Effects for NoopEffects {
    fn sling(&self, _convoy: &str, _member: &str) {}
    fn rotate(&self, _account: &str) {}
}

async fn boot(log: std::path::PathBuf) -> (String, gt_root::RootHandle<Arc<InMemoryBeads>>) {
    let (base, root, _audit) = boot_with_auth(log, AuthConfig::open()).await;
    (base, root)
}

async fn boot_with_auth(
    log: std::path::PathBuf,
    auth: AuthConfig,
) -> (
    String,
    gt_root::RootHandle<Arc<InMemoryBeads>>,
    InMemoryWebAudit,
) {
    let beads = Arc::new(InMemoryBeads::default());
    beads
        .upsert(&Bead::new("hq-x1", "demo bead", BeadStatus::Pending, 1))
        .await
        .unwrap();

    let sessions = Arc::new(InMemorySessions::default());
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
    };

    let audit = InMemoryWebAudit::new();
    let sink: Arc<dyn WebAuditSink> = Arc::new(audit.clone());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state, auth, sink, ReadinessGate::ready());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), root, audit)
}

#[tokio::test]
async fn snapshot_endpoints_serve_dto_rows() {
    let log = tempfile("snap");
    let (base, _root) = boot(log).await;

    let client = reqwest::Client::new();

    let beads: Vec<gt_web::dto::BeadDto> = client
        .get(format!("{base}/api/beads?status=pending"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(beads.len(), 1);
    assert_eq!(beads[0].id, "hq-x1");
    assert_eq!(beads[0].status, "pending");

    let sessions: Vec<gt_web::dto::SessionDto> = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(sessions.len(), 0);

    // Unknown status -> 400.
    let bad = client
        .get(format!("{base}/api/beads?status=bogus"))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);
}

#[tokio::test]
async fn sse_stream_delivers_event_driven_through_root() {
    let log = tempfile("sse");
    let (base, root) = boot(log).await;

    let client = reqwest::Client::new();

    // Open the SSE stream first; the broadcast hub is best-effort and only delivers events
    // emitted *after* a subscriber exists.
    let resp = client
        .get(format!("{base}/api/stream"))
        .header("accept", "text/event-stream")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let mut byte_stream = resp.bytes_stream();

    // Wait for the broadcast to actually register a subscriber before driving the event.
    for _ in 0..50 {
        if root.event_subscribers() >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(root.event_subscribers() >= 1, "SSE never subscribed");

    // Drive one event through the root via the agent relay (same path the reactor logs).
    root.agent_events
        .send(Envelope::root(AgentEvent::Spawned {
            session: "polecat-1".into(),
            rig: "granite".into(),
            role: SessionRole::Polecat,
            crew: None,
        }))
        .await
        .expect("agent relay");

    // Read until we see an `agent.spawned` SSE frame or time out.
    let mut buf = Vec::<u8>::new();
    let saw = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(chunk) = byte_stream.next().await {
            let bytes = chunk.unwrap();
            buf.extend_from_slice(&bytes);
            if let Ok(s) = std::str::from_utf8(&buf) {
                if s.contains("\"type\":\"agent.spawned\"") && s.contains("polecat-1") {
                    return true;
                }
            }
        }
        false
    })
    .await
    .unwrap_or(false);

    assert!(
        saw,
        "did not see agent.spawned in SSE stream; got: {}",
        String::from_utf8_lossy(&buf)
    );
}

#[tokio::test]
async fn nudge_emits_heartbeat_visible_via_sse() {
    let log = tempfile("nudge");
    let (base, root) = boot(log).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/stream"))
        .header("accept", "text/event-stream")
        .send()
        .await
        .unwrap();
    let mut bytes = resp.bytes_stream();

    for _ in 0..50 {
        if root.event_subscribers() >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // The agent actor needs to know about the session before we read it in replay; the SSE
    // assertion below only cares about the wire-level `agent.heartbeat` reaching the client.
    let agent = root.agent.clone();
    agent.add(Session::new("p9", "granite")).await;
    drop(agent);

    let resp = client
        .post(format!("{base}/api/nudge"))
        .json(&serde_json::json!({ "session": "p9" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let mut buf = Vec::<u8>::new();
    let saw = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(chunk) = bytes.next().await {
            let b = chunk.unwrap();
            buf.extend_from_slice(&b);
            if let Ok(s) = std::str::from_utf8(&buf) {
                if s.contains("\"type\":\"agent.heartbeat\"") && s.contains("\"p9\"") {
                    return true;
                }
            }
        }
        false
    })
    .await
    .unwrap_or(false);

    assert!(
        saw,
        "did not see agent.heartbeat in SSE stream; got: {}",
        String::from_utf8_lossy(&buf)
    );
}

fn tempfile(label: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("gt-web-test-{label}-{}.jsonl", ulid::Ulid::new()));
    p
}

/// hq-7pdl.2 gate: bearer middleware enforces auth and records both outcomes to the audit log.
/// Doc 07: IAM lives at the gateway; "who-consulted-what" lands in the shared event log.
#[tokio::test]
async fn bearer_auth_enforced_and_audited() {
    let log = tempfile("auth");
    let token = "shared-test-token";
    let (base, _root, audit) =
        boot_with_auth(log, AuthConfig::bearer(token.to_string())).await;

    let client = reqwest::Client::new();

    // Missing token -> 401, no handler reached.
    let r = client
        .get(format!("{base}/api/beads?status=pending"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);

    // Wrong token -> 401.
    let r = client
        .get(format!("{base}/api/beads?status=pending"))
        .header("authorization", "Bearer wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);

    // Correct token -> 200 + body served.
    let r = client
        .get(format!("{base}/api/beads?status=pending"))
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let beads: Vec<gt_web::dto::BeadDto> = r.json().await.unwrap();
    assert_eq!(beads.len(), 1);

    let recorded = audit.snapshot();
    assert!(
        recorded.iter().any(|e| matches!(
            e,
            WebAuditEvent::Unauthorized { reason, .. } if reason == "missing bearer token"
        )),
        "missing-token unauthorized audit row not recorded: {recorded:?}",
    );
    assert!(
        recorded.iter().any(|e| matches!(
            e,
            WebAuditEvent::Unauthorized { reason, .. } if reason == "invalid bearer token"
        )),
        "invalid-token unauthorized audit row not recorded: {recorded:?}",
    );
    assert!(
        recorded.iter().any(|e| matches!(
            e,
            WebAuditEvent::Invoked { status: 200, path, .. } if path == "/api/beads"
        )),
        "authorized invoked audit row not recorded: {recorded:?}",
    );
}
