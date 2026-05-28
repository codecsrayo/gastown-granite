//! Gate test for hq-mc72.10: the three tools that let the MCP surface drive the orchestrator
//! end-to-end without an out-of-band edge.
//!
//!  1. `scheduling.create_bead` mints a `pending` bead the dispatcher can claim, so
//!     `create_bead -> enqueue` ends in `Dispatched` instead of `DispatchFailed`.
//!  2. `agent.add` publishes `AgentEvent::Spawned` on the edge relay (the agent actor is
//!     relay-less by design), so a tool-driven add reaches the log/broadcast/projector.
//!  3. `quota.register` upserts an account so `sample`/`probe`/`rotate` have a window to act
//!     on — registration is not event-logged (edge config), but the snapshot now counts it.
//!
//! All three drive the shared `run_*` helpers directly — same code path as the generated
//! `#[tool]` methods, minus the wire transport.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use gt_agent::actor as agent_actor;
use gt_agent::{AddSession, AgentCommand, AgentEvent, SessionRole};
use gt_beads::{BeadRepository, BeadStatus, InMemoryBeads};
use gt_events::Envelope;
use gt_mcp::{
    audit::{AuditEvent, AuditSink, InMemoryAudit, Outcome},
    auth::Scope,
    CreateBead, CreateRig, McpService, RegisterAccount, SessionsRead,
};
use gt_quota::actor::{self as quota_actor, QuotaHandle};
use gt_scheduling::actor::{self as sched_actor, SchedHandle};
use gt_scheduling::SchedEvent;
use tokio::sync::mpsc;

/// Build a service over real domain actors. Returns the agent relay receiver so the agent
/// emission can be observed, plus the quota handle so its snapshot can be read.
#[allow(clippy::type_complexity)]
fn service(
    sched: SchedHandle,
    scope: Scope,
    audit: Arc<dyn AuditSink>,
) -> (
    McpService,
    mpsc::Receiver<Envelope<AgentEvent>>,
    QuotaHandle,
) {
    let agent = agent_actor::spawn(8);
    let (merge_tx, _merge_rx) = mpsc::channel(16);
    let merge = gt_merge::actor::spawn(gt_merge::InMemoryMergeRepo::default(), merge_tx);
    let (patrol_tx, _patrol_rx) = mpsc::channel(16);
    let patrol = gt_patrol::actor::spawn(gt_patrol::InMemoryPatrolRepo::default(), patrol_tx);
    let (orch_tx, _orch_rx) = mpsc::channel(16);
    let orch =
        gt_orchestration::actor::spawn(gt_orchestration::InMemoryOrchRepo::default(), orch_tx);
    let (quota_tx, _quota_rx) = mpsc::channel(16);
    let quota = quota_actor::spawn(quota_tx, HashMap::new());

    let (agent_tx, agent_rx) = mpsc::channel::<Envelope<AgentEvent>>(16);
    let svc = McpService::with_sessions(
        agent.clone(),
        SessionsRead::Actor(agent),
        merge,
        sched,
        patrol,
        orch,
        quota.clone(),
        scope,
        audit,
        Some(agent_tx),
        None, // rig_creator: tests don't shell out to `gt`
    );
    (svc, agent_rx, quota)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_bead_then_enqueue_dispatches() {
    let repo = Arc::new(InMemoryBeads::default());
    let (sched_tx, mut sched_rx) = mpsc::channel::<Envelope<SchedEvent>>(64);
    let sched = sched_actor::spawn(repo.clone(), sched_tx, 1);
    let audit = Arc::new(InMemoryAudit::new());
    let (svc, _agent_rx, _quota) = service(
        sched,
        Scope::admin("max"),
        Arc::clone(&audit) as Arc<dyn AuditSink>,
    );

    svc.run_create_bead(
        "scheduling.create_bead.execute",
        CreateBead {
            id: "hq-x".into(),
            title: "auto work".into(),
            priority: 0,
        },
        false,
    )
    .await
    .expect("create_bead.execute should succeed");

    // The bead now exists as pending in the repo (no SchedEvent — it is a repo write).
    let bead = repo
        .get("hq-x")
        .await
        .unwrap()
        .expect("bead must exist after create");
    assert_eq!(bead.status, BeadStatus::Pending);

    // enqueue → the pump CAS-claims the freshly created bead → Dispatched, not DispatchFailed.
    svc.run_sched(
        "scheduling.enqueue.execute",
        json!({"bead": "hq-x", "priority": 0}),
        gt_scheduling::SchedCommand::Enqueue(gt_scheduling::Enqueue {
            bead: "hq-x".into(),
            priority: 0,
        }),
        false,
    )
    .await
    .expect("enqueue.execute should succeed");

    let mut events = Vec::new();
    while let Ok(Some(env)) =
        tokio::time::timeout(Duration::from_millis(300), sched_rx.recv()).await
    {
        events.push(env.payload);
    }
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SchedEvent::Dispatched { bead, .. } if bead == "hq-x")),
        "the created bead must be dispatched, not failed: {events:?}",
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, SchedEvent::DispatchFailed { bead, .. } if bead == "hq-x")),
        "no DispatchFailed expected once the bead exists: {events:?}",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_add_emits_spawned_on_the_relay() {
    let repo = Arc::new(InMemoryBeads::default());
    let (sched_tx, _sched_rx) = mpsc::channel::<Envelope<SchedEvent>>(16);
    let sched = sched_actor::spawn(repo, sched_tx, 1);
    let audit = Arc::new(InMemoryAudit::new());
    let (svc, mut agent_rx, _quota) = service(
        sched,
        Scope::admin("max"),
        Arc::clone(&audit) as Arc<dyn AuditSink>,
    );

    svc.run(
        "agent.add.execute",
        json!({"id": "sess-1", "rig": "plane"}),
        AgentCommand::Add(AddSession {
            id: "sess-1".into(),
            rig: "plane".into(),
        }),
        false,
    )
    .await
    .expect("agent.add.execute should succeed");

    let env = tokio::time::timeout(Duration::from_millis(300), agent_rx.recv())
        .await
        .expect("relay must receive an event within the timeout")
        .expect("relay channel must stay open");
    match env.payload {
        AgentEvent::Spawned {
            session,
            rig,
            role,
            crew,
        } => {
            assert_eq!(session, "sess-1");
            assert_eq!(rig, "plane");
            assert_eq!(role, SessionRole::Polecat);
            assert_eq!(crew, None);
        }
        other => panic!("expected Spawned, got {other:?}"),
    }

    // validate must NOT emit (no state change, no event).
    svc.run(
        "agent.add.validate",
        json!({"id": "sess-2", "rig": "plane"}),
        AgentCommand::Add(AddSession {
            id: "sess-2".into(),
            rig: "plane".into(),
        }),
        true,
    )
    .await
    .expect("agent.add.validate should succeed");
    assert!(
        tokio::time::timeout(Duration::from_millis(150), agent_rx.recv())
            .await
            .is_err(),
        "validate must not emit a Spawned event",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quota_register_makes_account_visible() {
    let repo = Arc::new(InMemoryBeads::default());
    let (sched_tx, _sched_rx) = mpsc::channel::<Envelope<SchedEvent>>(16);
    let sched = sched_actor::spawn(repo, sched_tx, 1);
    let audit = Arc::new(InMemoryAudit::new());
    let (svc, _agent_rx, quota) = service(
        sched,
        Scope::admin("max"),
        Arc::clone(&audit) as Arc<dyn AuditSink>,
    );

    let (accounts_before, _) = quota.snapshot().await;
    assert_eq!(accounts_before, 0, "registry starts empty");

    svc.run_quota_register(
        "quota.register.execute",
        RegisterAccount {
            account: "acct-1".into(),
            limit: 1000,
            started_at_secs: 1000,
            resets_at_secs: 19000,
            weekly: false,
        },
        false,
    )
    .await
    .expect("quota.register.execute should succeed");

    let (accounts_after, _) = quota.snapshot().await;
    assert_eq!(
        accounts_after, 1,
        "registered account must show in the snapshot"
    );

    // Bad input is rejected and audited as a failed invocation.
    let bad = svc
        .run_quota_register(
            "quota.register.execute",
            RegisterAccount {
                account: "".into(),
                limit: 0,
                started_at_secs: 10,
                resets_at_secs: 5,
                weekly: false,
            },
            false,
        )
        .await;
    assert!(bad.is_err(), "empty/invalid registration must fail");
    assert!(
        audit.snapshot().iter().any(|e| matches!(
            e,
            AuditEvent::Invoked { tool, outcome: Outcome::Failed { .. }, .. }
                if tool == "quota.register.execute"
        )),
        "the rejected registration must be audited as a failed invocation",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rig_create_validates_and_reports_unconfigured() {
    let repo = Arc::new(InMemoryBeads::default());
    let (sched_tx, _sched_rx) = mpsc::channel::<Envelope<SchedEvent>>(16);
    let sched = sched_actor::spawn(repo, sched_tx, 1);
    let audit = Arc::new(InMemoryAudit::new());
    // Built via `service` (McpService::new) → no RigCreator wired.
    let (svc, _agent_rx, _quota) =
        service(sched, Scope::admin("max"), Arc::clone(&audit) as Arc<dyn AuditSink>);

    // Bad name (would be a shell/path hazard if not validated) is rejected.
    let bad = svc
        .run_rig_create(
            "rig.create.validate",
            CreateRig { name: "../evil; rm".into(), git_url: "x".into(), prefix: None },
            true,
        )
        .await;
    assert!(bad.is_err(), "invalid rig name must be rejected");

    // Valid args, validate-only: ok without touching anything.
    svc.run_rig_create(
        "rig.create.validate",
        CreateRig { name: "demo-rig".into(), git_url: "file:///tmp/x".into(), prefix: None },
        true,
    )
    .await
    .expect("valid rig.create.validate should pass");

    // Execute with no RigCreator wired → clean error, audited as failed (not a panic/hang).
    let unconfigured = svc
        .run_rig_create(
            "rig.create.execute",
            CreateRig { name: "demo-rig".into(), git_url: "file:///tmp/x".into(), prefix: None },
            false,
        )
        .await;
    assert!(unconfigured.is_err(), "execute without GT_BIN must error");
    assert!(
        audit.snapshot().iter().any(|e| matches!(
            e,
            AuditEvent::Invoked { tool, outcome: Outcome::Failed { .. }, .. }
                if tool == "rig.create.execute"
        )),
        "the unconfigured execute must be audited as a failed invocation",
    );
}
