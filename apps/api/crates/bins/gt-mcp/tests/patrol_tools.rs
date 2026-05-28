//! Gate test for Paso 6.f.7: the patrol MCP tools drive the **shared** lease-tracker actor
//! through the `Command { validate, execute }` path. Register opens a lease, a Tick past the
//! timeout emits `LeaseExpired` (variadic output relayed by the actor), and scope/audit are
//! enforced at the frontier — the same relay the composition root drains.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use gt_agent::actor as agent_actor;
use gt_events::Envelope;
use gt_mcp::{
    audit::{AuditEvent, AuditSink, InMemoryAudit, Outcome},
    auth::Scope,
    McpService,
};
use gt_patrol::actor::{self as patrol_actor, PatrolHandle};
use gt_patrol::{CloseLease, Heartbeat, PatrolCommand, PatrolEvent, RegisterLease, Tick};
use tokio::sync::mpsc;

/// Collect every event the patrol relays until it goes quiet for 200ms.
async fn drain(rx: &mut mpsc::Receiver<Envelope<PatrolEvent>>) -> Vec<PatrolEvent> {
    let mut out = Vec::new();
    while let Ok(Some(env)) = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
        out.push(env.payload);
    }
    out
}

/// Build a service driven through the patrol tools; throwaway agent/merge/scheduling actors
/// fill out the surface.
fn service(patrol: PatrolHandle, scope: Scope, audit: Arc<dyn AuditSink>) -> McpService {
    let (merge_tx, _merge_rx) = mpsc::channel(16);
    let merge = gt_merge::actor::spawn(merge_tx);
    let (sched_tx, _sched_rx) = mpsc::channel(16);
    let sched =
        gt_scheduling::actor::spawn(Arc::new(gt_beads::InMemoryBeads::default()), sched_tx, 4);
    McpService::new(agent_actor::spawn(8), merge, sched, patrol, scope, audit)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn patrol_tools_drive_shared_actor_register_then_tick_expires() {
    let (tx, mut rx) = mpsc::channel::<Envelope<PatrolEvent>>(64);
    let patrol = patrol_actor::spawn(tx);

    let audit = Arc::new(InMemoryAudit::new());
    let svc = service(patrol, Scope::admin("max"), Arc::clone(&audit) as Arc<dyn AuditSink>);

    // register a lease at t=10.
    svc.run_patrol(
        "patrol.register.execute",
        json!({"bead": "b1", "worker": "w1", "priority": 1, "now_secs": 10}),
        PatrolCommand::Register(RegisterLease {
            bead: "b1".into(),
            worker: "w1".into(),
            priority: 1,
            now_secs: 10,
        }),
        false,
    )
    .await
    .expect("register.execute should succeed");

    // tick at t=20, timeout 30 → age 10, not expired (no LeaseExpired).
    svc.run_patrol(
        "patrol.tick.execute",
        json!({"now_secs": 20, "timeout_secs": 30}),
        PatrolCommand::Tick(Tick { now_secs: 20, timeout_secs: 30 }),
        false,
    )
    .await
    .expect("tick.execute should succeed");

    // tick at t=60, timeout 30 → age 50 > 30 → LeaseExpired(b1, w1).
    svc.run_patrol(
        "patrol.tick.execute",
        json!({"now_secs": 60, "timeout_secs": 30}),
        PatrolCommand::Tick(Tick { now_secs: 60, timeout_secs: 30 }),
        false,
    )
    .await
    .expect("tick.execute should succeed");

    let events = drain(&mut rx).await;
    assert!(
        events.iter().any(|e| matches!(e, PatrolEvent::LeaseRegistered { bead, worker, .. } if bead == "b1" && worker == "w1")),
        "register.execute must emit LeaseRegistered: {events:?}",
    );
    let expired: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, PatrolEvent::LeaseExpired { .. }))
        .collect();
    assert_eq!(expired.len(), 1, "exactly one LeaseExpired across the two ticks: {events:?}");
    assert!(
        matches!(expired[0], PatrolEvent::LeaseExpired { bead, worker, .. } if bead == "b1" && worker == "w1"),
        "expired lease must be b1/w1: {expired:?}",
    );

    let recorded = audit.snapshot();
    assert_eq!(
        recorded
            .iter()
            .filter(|e| matches!(e, AuditEvent::Invoked { tool, outcome: Outcome::Ok, .. } if tool == "patrol.tick.execute"))
            .count(),
        2,
        "both ticks audited Ok: {recorded:?}",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn patrol_heartbeat_prevents_expiry_and_scope_blocks_execute() {
    let (tx, mut rx) = mpsc::channel::<Envelope<PatrolEvent>>(64);
    let patrol = patrol_actor::spawn(tx);
    let audit = Arc::new(InMemoryAudit::new());
    let admin = service(
        patrol.clone(),
        Scope::admin("max"),
        Arc::clone(&audit) as Arc<dyn AuditSink>,
    );

    admin
        .run_patrol(
            "patrol.register.execute",
            json!({"bead": "b1", "worker": "w1", "priority": 0, "now_secs": 0}),
            PatrolCommand::Register(RegisterLease {
                bead: "b1".into(),
                worker: "w1".into(),
                priority: 0,
                now_secs: 0,
            }),
            false,
        )
        .await
        .unwrap();
    // heartbeat at t=40 refreshes the lease.
    admin
        .run_patrol(
            "patrol.heartbeat.execute",
            json!({"worker": "w1", "now_secs": 40}),
            PatrolCommand::Heartbeat(Heartbeat { worker: "w1".into(), now_secs: 40 }),
            false,
        )
        .await
        .unwrap();
    // tick at t=50, timeout 30 → without the heartbeat age would be 50; refreshed → age 10, no expiry.
    admin
        .run_patrol(
            "patrol.tick.execute",
            json!({"now_secs": 50, "timeout_secs": 30}),
            PatrolCommand::Tick(Tick { now_secs: 50, timeout_secs: 30 }),
            false,
        )
        .await
        .unwrap();
    // close the lease.
    admin
        .run_patrol(
            "patrol.close.execute",
            json!({"bead": "b1"}),
            PatrolCommand::Close(CloseLease { bead: "b1".into() }),
            false,
        )
        .await
        .unwrap();

    let events = drain(&mut rx).await;
    assert!(
        !events.iter().any(|e| matches!(e, PatrolEvent::LeaseExpired { .. })),
        "heartbeat must keep the lease alive — no expiry: {events:?}",
    );
    assert!(
        events.iter().any(|e| matches!(e, PatrolEvent::LeaseClosed { bead } if bead == "b1")),
        "close.execute must emit LeaseClosed: {events:?}",
    );

    // read_only scope rejects execute, audited Unauthorized.
    let watcher_audit = Arc::new(InMemoryAudit::new());
    let watcher = service(
        patrol,
        Scope::read_only("watcher"),
        Arc::clone(&watcher_audit) as Arc<dyn AuditSink>,
    );
    let denied = watcher
        .run_patrol(
            "patrol.tick.execute",
            json!({"now_secs": 1, "timeout_secs": 1}),
            PatrolCommand::Tick(Tick { now_secs: 1, timeout_secs: 1 }),
            false,
        )
        .await;
    assert!(denied.is_err(), "read_only must reject patrol.tick.execute");
    assert!(
        watcher_audit.snapshot().iter().any(|e| matches!(
            e,
            AuditEvent::Unauthorized { tool, .. } if tool == "patrol.tick.execute"
        )),
        "rejection must be audited Unauthorized",
    );
}
