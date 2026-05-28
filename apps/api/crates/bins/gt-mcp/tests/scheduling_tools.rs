//! Gate test for Paso 6.f.5: the scheduling MCP tools drive the **shared** dispatcher actor
//! (not an isolated copy) through the `Command { validate, execute }` path, and a successful
//! `execute` emits the matching `SchedEvent` to the actor's relay — the same relay the
//! composition root drains into the audit log.
//!
//! Drives `McpService::run_sched` directly (the shared dispatch behind the generated
//! `#[tool]` methods), so the scope + audit + actor path is covered without the wire
//! transport.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use gt_agent::actor as agent_actor;
use gt_beads::{Bead, BeadRepository, BeadStatus, InMemoryBeads};
use gt_events::Envelope;
use gt_mcp::{
    audit::{AuditEvent, AuditSink, InMemoryAudit, Outcome},
    auth::Scope,
    McpService,
};
use gt_scheduling::actor::{self as sched_actor, SchedHandle};
use gt_scheduling::{Enqueue, MarkDispatched, SchedCommand, SchedEvent};
use tokio::sync::mpsc;

/// Collect every event the dispatcher relays until it goes quiet for 200ms.
async fn drain(rx: &mut mpsc::Receiver<Envelope<SchedEvent>>) -> Vec<SchedEvent> {
    let mut out = Vec::new();
    while let Ok(Some(env)) = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
        out.push(env.payload);
    }
    out
}

/// Build a service exposing the full domain surface but driven through the scheduling tools.
/// Throwaway agent + merge + patrol + orch + quota actors are wired alongside the scheduler.
fn service(sched: SchedHandle, scope: Scope, audit: Arc<dyn AuditSink>) -> McpService {
    let (merge_tx, _merge_rx) = mpsc::channel(16);
    let merge = gt_merge::actor::spawn(gt_merge::InMemoryMergeRepo::default(), merge_tx);
    let (patrol_tx, _patrol_rx) = mpsc::channel(16);
    let patrol = gt_patrol::actor::spawn(gt_patrol::InMemoryPatrolRepo::default(), patrol_tx);
    let (orch_tx, _orch_rx) = mpsc::channel(16);
    let orch = gt_orchestration::actor::spawn(gt_orchestration::InMemoryOrchRepo::default(), orch_tx);
    let (quota_tx, _quota_rx) = mpsc::channel(16);
    let quota = gt_quota::actor::spawn(quota_tx, std::collections::HashMap::new());
    McpService::new(agent_actor::spawn(8), merge, sched, patrol, orch, quota, scope, audit)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scheduling_tools_drive_shared_actor_and_emit_events() {
    let (tx, mut rx) = mpsc::channel::<Envelope<SchedEvent>>(64);
    let repo = Arc::new(InMemoryBeads::default());
    repo.upsert(&Bead::new("b1", "work", BeadStatus::Pending, 1))
        .await
        .unwrap();
    // Capacity 2 so the pump can claim b1 after enqueue and a manual mark still fits.
    let sched = sched_actor::spawn(repo.clone(), tx, 2);

    let audit = Arc::new(InMemoryAudit::new());
    let svc = service(sched, Scope::admin("max"), Arc::clone(&audit) as Arc<dyn AuditSink>);

    // enqueue.execute → Enqueue emitted; the pump then claims the pending bead → Dispatched.
    svc.run_sched(
        "scheduling.enqueue.execute",
        json!({"bead": "b1", "priority": 1}),
        SchedCommand::Enqueue(Enqueue { bead: "b1".into(), priority: 1 }),
        false,
    )
    .await
    .expect("enqueue.execute should succeed");

    // mark_dispatched.execute → Dispatched for a manually assigned bead/worker.
    svc.run_sched(
        "scheduling.mark_dispatched.execute",
        json!({"bead": "b2", "worker": "w9"}),
        SchedCommand::MarkDispatched(MarkDispatched { bead: "b2".into(), worker: "w9".into() }),
        false,
    )
    .await
    .expect("mark_dispatched.execute should succeed");

    let events = drain(&mut rx).await;

    assert!(
        events.iter().any(|e| matches!(e, SchedEvent::Enqueue { bead, priority } if bead == "b1" && *priority == 1)),
        "enqueue.execute must emit SchedEvent::Enqueue: {events:?}",
    );
    assert!(
        events.iter().any(|e| matches!(e, SchedEvent::Dispatched { bead, .. } if bead == "b1")),
        "the pump must claim the pending bead and emit Dispatched(b1): {events:?}",
    );
    assert!(
        events.iter().any(|e| matches!(e, SchedEvent::Dispatched { bead, worker } if bead == "b2" && worker == "w9")),
        "mark_dispatched.execute must emit Dispatched(b2, w9): {events:?}",
    );

    let recorded = audit.snapshot();
    assert!(
        recorded.iter().any(|e| matches!(
            e,
            AuditEvent::Invoked { tool, outcome: Outcome::Ok, .. } if tool == "scheduling.enqueue.execute"
        )),
        "audit missing ok enqueue.execute: {recorded:?}",
    );
    assert!(
        recorded.iter().any(|e| matches!(
            e,
            AuditEvent::Invoked { tool, outcome: Outcome::Ok, .. } if tool == "scheduling.mark_dispatched.execute"
        )),
        "audit missing ok mark_dispatched.execute: {recorded:?}",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scheduling_scope_and_capacity_rejections() {
    // read_only scope: execute is blocked before dispatch and audited Unauthorized.
    let (tx, _rx) = mpsc::channel::<Envelope<SchedEvent>>(16);
    let repo = Arc::new(InMemoryBeads::default());
    let sched = sched_actor::spawn(repo, tx, 1);
    let audit = Arc::new(InMemoryAudit::new());
    let watcher = service(
        sched,
        Scope::read_only("watcher"),
        Arc::clone(&audit) as Arc<dyn AuditSink>,
    );

    let denied = watcher
        .run_sched(
            "scheduling.enqueue.execute",
            json!({"bead": "b1", "priority": 0}),
            SchedCommand::Enqueue(Enqueue { bead: "b1".into(), priority: 0 }),
            false,
        )
        .await;
    assert!(denied.is_err(), "read_only must reject enqueue.execute");
    assert!(
        audit.snapshot().iter().any(|e| matches!(
            e,
            AuditEvent::Unauthorized { tool, .. } if tool == "scheduling.enqueue.execute"
        )),
        "rejection must be audited Unauthorized",
    );

    // Capacity 0: mark_dispatched.validate fails cleanly (no slot), audited Invoked { Failed }.
    let (tx0, _rx0) = mpsc::channel::<Envelope<SchedEvent>>(16);
    let repo0 = Arc::new(InMemoryBeads::default());
    let sched0 = sched_actor::spawn(repo0, tx0, 0);
    let admin_audit = Arc::new(InMemoryAudit::new());
    let admin = service(
        sched0,
        Scope::admin("max"),
        Arc::clone(&admin_audit) as Arc<dyn AuditSink>,
    );

    let no_capacity = admin
        .run_sched(
            "scheduling.mark_dispatched.validate",
            json!({"bead": "b1", "worker": "w1"}),
            SchedCommand::MarkDispatched(MarkDispatched { bead: "b1".into(), worker: "w1".into() }),
            true,
        )
        .await;
    assert!(no_capacity.is_err(), "mark_dispatched must fail with no capacity");
    assert!(
        admin_audit.snapshot().iter().any(|e| matches!(
            e,
            AuditEvent::Invoked { tool, outcome: Outcome::Failed { .. }, .. }
                if tool == "scheduling.mark_dispatched.validate"
        )),
        "capacity failure must be audited as a failed invocation",
    );
}
