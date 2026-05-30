//! Paso 8.7 gate (hq-8iur.7): SessionRole + crew flow end to end.
//!
//! Part 1 (HTTP): seed the read-side with a Mayor + Polecat + Witness, then assert
//! `GET /api/sessions` returns all three with distinct `role`, and `?role=polecat` narrows
//! to exactly one. Crew is surfaced for the polecat.
//!
//! Part 2 (replay): `AgentEvent::Spawned` carries role + crew as event data (no clock), so
//! the pure reducer rebuilds the same `SessionRegistry` byte-for-byte across runs — the
//! determinism rule still holds with the new fields.

use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;

use gt_agent::{
    AgentEvent, DogKind, InMemorySessions, Session, SessionRegistry, SessionRole,
};
use gt_audit::{replay, EventRecord};
use gt_beads::InMemoryBeads;
use gt_events::Envelope;
use gt_root::{root::Effects, spawn, RootConfig, RootHandle, SystemClock};
use gt_web::{router, AppState, AuthConfig, InMemoryWebAudit, ReadinessGate, WebAuditSink};

struct NoopEffects;
impl Effects for NoopEffects {
    fn sling(&self, _convoy: &str, _member: &str) {}
    fn rotate(&self, _account: &str) {}
}

async fn boot(sessions: Vec<Session>) -> (String, RootHandle<Arc<InMemoryBeads>>) {
    let beads = Arc::new(InMemoryBeads::default());
    let sessions = Arc::new(InMemorySessions::new(sessions));
    let log = std::env::temp_dir().join(format!("gt-web-role-{}.jsonl", ulid::Ulid::new()));
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
        town_root: None,
        issues: None,
        bus: None,
        worktrees_stream: None,
        control: None,
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
async fn api_sessions_exposes_role_and_filters() {
    // A town with a mayor, a polecat (crew "atom" running inside), and a rig witness dog.
    let fixture = vec![
        Session::with_role("mayor", "town", SessionRole::Mayor, None),
        Session::with_role(
            "gt-granite-atom",
            "granite",
            SessionRole::Polecat,
            Some("atom".into()),
        ),
        Session::with_role(
            "gt-granite-witness",
            "granite",
            SessionRole::Dog(DogKind::Witness),
            None,
        ),
    ];
    let (base, root) = boot(fixture).await;
    let client = reqwest::Client::new();

    // All three active sessions, distinct roles.
    let all: Vec<gt_web::dto::SessionDto> = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(all.len(), 3, "three active sessions");
    let mut roles: Vec<&str> = all.iter().map(|s| s.role.as_str()).collect();
    roles.sort();
    assert_eq!(roles, vec!["mayor", "polecat", "witness"], "distinct roles");

    // Crew is surfaced for the polecat only.
    let polecat = all.iter().find(|s| s.role == "polecat").unwrap();
    assert_eq!(polecat.crew.as_deref(), Some("atom"));
    let mayor = all.iter().find(|s| s.role == "mayor").unwrap();
    assert_eq!(mayor.crew, None);

    // ?role=polecat narrows to exactly one.
    let polecats: Vec<gt_web::dto::SessionDto> = client
        .get(format!("{base}/api/sessions?role=polecat"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(polecats.len(), 1);
    assert_eq!(polecats[0].id, "gt-granite-atom");

    // ?role=witness narrows to the dog.
    let witnesses: Vec<gt_web::dto::SessionDto> = client
        .get(format!("{base}/api/sessions?role=witness"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(witnesses.len(), 1);
    assert_eq!(witnesses[0].id, "gt-granite-witness");

    // An unknown role is a view that matches nothing (not an error).
    let unknown = client
        .get(format!("{base}/api/sessions?role=nope"))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), 200);
    let none: Vec<gt_web::dto::SessionDto> = unknown.json().await.unwrap();
    assert!(none.is_empty());

    // Give the spawned server a beat, then tear down.
    tokio::time::sleep(Duration::from_millis(10)).await;
    root.shutdown();
}

#[test]
fn replay_rebuilds_sessions_with_roles_byte_identical() {
    // Spawn a mayor, a polecat (with crew) and a witness as events.
    let events = vec![
        AgentEvent::Spawned {
            session: "mayor".into(),
            rig: "town".into(),
            role: SessionRole::Mayor,
            crew: None,
        },
        AgentEvent::Spawned {
            session: "gt-granite-atom".into(),
            rig: "granite".into(),
            role: SessionRole::Polecat,
            crew: Some("atom".into()),
        },
        AgentEvent::Spawned {
            session: "gt-granite-witness".into(),
            rig: "granite".into(),
            role: SessionRole::Dog(DogKind::Witness),
            crew: None,
        },
    ];
    let records: Vec<EventRecord> = events
        .iter()
        .map(|e| EventRecord::from_envelope(&Envelope::root(e.clone())).unwrap())
        .collect();

    // Live fold and two replays must all agree byte-for-byte.
    let mut live = SessionRegistry::default();
    for e in &events {
        live.apply(e);
    }
    let a = replay(&records, SessionRegistry::default(), |r, e: &AgentEvent| r.apply(e)).unwrap();
    let b = replay(&records, SessionRegistry::default(), |r, e: &AgentEvent| r.apply(e)).unwrap();
    assert_eq!(live.fingerprint(), a.fingerprint());
    assert_eq!(a.fingerprint(), b.fingerprint());

    // Roles + crew are reconstructed, not lost.
    assert_eq!(a.get("mayor").unwrap().role, SessionRole::Mayor);
    let pc = a.get("gt-granite-atom").unwrap();
    assert_eq!(pc.role, SessionRole::Polecat);
    assert_eq!(pc.crew.as_deref(), Some("atom"));
    assert_eq!(
        a.get("gt-granite-witness").unwrap().role,
        SessionRole::Dog(DogKind::Witness)
    );

    // Legacy event without role/crew (pre-8.7) still decodes → defaults to polecat.
    let legacy = serde_json::json!({
        "Spawned": { "session": "old", "rig": "granite" }
    });
    let decoded: AgentEvent = serde_json::from_value(legacy).expect("legacy Spawned decodes");
    let mut reg = SessionRegistry::default();
    reg.apply(&decoded);
    assert_eq!(reg.get("old").unwrap().role, SessionRole::Polecat);
}
