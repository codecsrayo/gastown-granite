//! Gate test for Paso 6.f.9: the orchestration MCP tools drive the **shared** convoy actor
//! through the `Command { validate, execute }` path. A launch creates + launches + hands off
//! the first member (variadic output relayed by the actor), completing members advances the
//! sequential handoff to close, and a member failure halts the convoy. Scope + audit are
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
use gt_orchestration::actor::{self as orch_actor, OrchHandle};
use gt_orchestration::{CompleteMember, FailMember, LaunchConvoy, OrchCommand, OrchEvent};
use tokio::sync::mpsc;

/// Collect every event the orchestration actor relays until it goes quiet for 200ms.
async fn drain(rx: &mut mpsc::Receiver<Envelope<OrchEvent>>) -> Vec<OrchEvent> {
    let mut out = Vec::new();
    while let Ok(Some(env)) = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
        out.push(env.payload);
    }
    out
}

/// Build a service driven through the orch tools; throwaway agent/merge/scheduling/patrol
/// actors fill out the surface.
fn service(orch: OrchHandle, scope: Scope, audit: Arc<dyn AuditSink>) -> McpService {
    let (merge_tx, _merge_rx) = mpsc::channel(16);
    let merge = gt_merge::actor::spawn(merge_tx);
    let (sched_tx, _sched_rx) = mpsc::channel(16);
    let sched =
        gt_scheduling::actor::spawn(Arc::new(gt_beads::InMemoryBeads::default()), sched_tx, 4);
    let (patrol_tx, _patrol_rx) = mpsc::channel(16);
    let patrol = gt_patrol::actor::spawn(patrol_tx);
    let (quota_tx, _quota_rx) = mpsc::channel(16);
    let quota = gt_quota::actor::spawn(quota_tx, std::collections::HashMap::new());
    McpService::new(agent_actor::spawn(8), merge, sched, patrol, orch, quota, scope, audit)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn orch_tools_launch_handoff_close() {
    let (tx, mut rx) = mpsc::channel::<Envelope<OrchEvent>>(64);
    let orch = orch_actor::spawn(tx);
    let audit = Arc::new(InMemoryAudit::new());
    let svc = service(orch, Scope::admin("max"), Arc::clone(&audit) as Arc<dyn AuditSink>);

    // launch a 2-member convoy → created + launched + first member dispatched.
    svc.run_orch(
        "orch.launch_convoy.execute",
        json!({"convoy": "cv", "members": ["a", "b"]}),
        OrchCommand::Launch(LaunchConvoy { convoy: "cv".into(), members: vec!["a".into(), "b".into()] }),
        false,
    )
    .await
    .expect("launch_convoy.execute should succeed");

    // complete a → b dispatched.
    svc.run_orch(
        "orch.complete_member.execute",
        json!({"convoy": "cv", "member": "a"}),
        OrchCommand::Complete(CompleteMember { convoy: "cv".into(), member: "a".into() }),
        false,
    )
    .await
    .expect("complete a should succeed");

    // complete b → convoy closes.
    svc.run_orch(
        "orch.complete_member.execute",
        json!({"convoy": "cv", "member": "b"}),
        OrchCommand::Complete(CompleteMember { convoy: "cv".into(), member: "b".into() }),
        false,
    )
    .await
    .expect("complete b should succeed");

    let events = drain(&mut rx).await;
    let dispatched: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            OrchEvent::MemberDispatched { member, .. } => Some(member.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(dispatched, vec!["a", "b"], "sequential handoff a then b: {events:?}");
    assert!(
        events.iter().any(|e| matches!(e, OrchEvent::ConvoyClosed { convoy } if convoy == "cv")),
        "convoy must close after all members complete: {events:?}",
    );

    let ok_execs = audit
        .snapshot()
        .into_iter()
        .filter(|e| matches!(e, AuditEvent::Invoked { outcome: Outcome::Ok, .. }))
        .count();
    assert_eq!(ok_execs, 3, "three successful orch executes audited");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn orch_fail_member_halts_and_scope_blocks_execute() {
    let (tx, mut rx) = mpsc::channel::<Envelope<OrchEvent>>(64);
    let orch = orch_actor::spawn(tx);
    let audit = Arc::new(InMemoryAudit::new());
    let admin = service(orch.clone(), Scope::admin("max"), Arc::clone(&audit) as Arc<dyn AuditSink>);

    admin
        .run_orch(
            "orch.launch_convoy.execute",
            json!({"convoy": "cv", "members": ["a", "b"]}),
            OrchCommand::Launch(LaunchConvoy { convoy: "cv".into(), members: vec!["a".into(), "b".into()] }),
            false,
        )
        .await
        .unwrap();
    admin
        .run_orch(
            "orch.fail_member.execute",
            json!({"convoy": "cv", "member": "a", "reason": "boom"}),
            OrchCommand::Fail(FailMember { convoy: "cv".into(), member: "a".into(), reason: "boom".into() }),
            false,
        )
        .await
        .unwrap();

    let events = drain(&mut rx).await;
    assert!(
        events.iter().any(|e| matches!(e, OrchEvent::ConvoyFailed { convoy, member, .. } if convoy == "cv" && member == "a")),
        "fail_member must halt the convoy: {events:?}",
    );
    assert!(
        !events.iter().any(|e| matches!(e, OrchEvent::MemberDispatched { member, .. } if member == "b")),
        "b must never be dispatched after the convoy halts: {events:?}",
    );

    // read_only scope rejects execute, audited Unauthorized.
    let watcher_audit = Arc::new(InMemoryAudit::new());
    let watcher = service(orch, Scope::read_only("watcher"), Arc::clone(&watcher_audit) as Arc<dyn AuditSink>);
    let denied = watcher
        .run_orch(
            "orch.launch_convoy.execute",
            json!({"convoy": "cv2", "members": ["x"]}),
            OrchCommand::Launch(LaunchConvoy { convoy: "cv2".into(), members: vec!["x".into()] }),
            false,
        )
        .await;
    assert!(denied.is_err(), "read_only must reject orch.launch_convoy.execute");
    assert!(
        watcher_audit.snapshot().iter().any(|e| matches!(
            e,
            AuditEvent::Unauthorized { tool, .. } if tool == "orch.launch_convoy.execute"
        )),
        "rejection must be audited Unauthorized",
    );
}
