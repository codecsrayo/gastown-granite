//! Gate test for Paso 6.f.2: gt-mcp drives the agent domain end-to-end through the
//! `Command { validate, execute }` path via the rmcp tool router.
//!
//! `McpService::run` is the shared dispatch backing every `#[tool]` method generated
//! by `#[tool_router]`. By driving `run` directly the test covers the same scope +
//! audit + actor path that the wire transport hits, without standing up stdio.
//!
//! Covers:
//! - A `read_only` scope rejects `.execute` and records `Unauthorized`.
//! - A `validate` call against a missing session fails cleanly and is audited as
//!   `Invoked { Failed }`.
//! - A full-scope `execute` transitions `Spawned → Working` and is audited `Ok`.
//! - An illegal transition (`Working → Spawned`) is rejected by the state machine
//!   and audited `Failed`.
//! - The actor snapshot reflects exactly one state advance.

use std::sync::Arc;

use serde_json::json;

use gt_agent::actor;
use gt_agent::{AddSession, AgentCommand, SessionState, TransitionSession};
use gt_mcp::{
    audit::{AuditEvent, AuditSink, InMemoryAudit, Outcome},
    auth::Scope,
    McpService,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_gate_validate_only_blocks_execute_full_scope_drives_state() {
    let agent = actor::spawn(16);
    // This gate exercises only the agent tools; a merge actor is wired so the service carries
    // its full domain surface. Keep the relay receiver alive so the actor's sends don't error.
    let (merge_tx, _merge_rx) = tokio::sync::mpsc::channel(16);
    let merge = gt_merge::actor::spawn(merge_tx);

    let watcher_audit = Arc::new(InMemoryAudit::new());
    let watcher = McpService::new(
        agent.clone(),
        merge.clone(),
        Scope::read_only("watcher"),
        Arc::clone(&watcher_audit) as Arc<dyn AuditSink>,
    );

    let admin_audit = Arc::new(InMemoryAudit::new());
    let admin = McpService::new(
        agent.clone(),
        merge.clone(),
        Scope::admin("admin"),
        Arc::clone(&admin_audit) as Arc<dyn AuditSink>,
    );

    // watcher tries to add a session → blocked by scope; audited as Unauthorized.
    let add_args = AddSession { id: "p1".into(), rig: "granite".into() };
    let denied = watcher
        .run(
            "agent.add.execute",
            json!({"id": "p1", "rig": "granite"}),
            AgentCommand::Add(add_args.clone()),
            false,
        )
        .await;
    let denied_msg = denied.expect_err("execute on read_only must fail").message.to_string();
    assert!(denied_msg.contains("validate_only"), "denied msg: {denied_msg}");

    // watcher validates a missing session → fails cleanly, Invoked { Failed }.
    let validate_missing = watcher
        .run(
            "agent.transition.validate",
            json!({"id": "ghost", "to": "working"}),
            AgentCommand::Transition(TransitionSession {
                id: "ghost".into(),
                to: SessionState::Working,
            }),
            true,
        )
        .await;
    assert!(validate_missing.is_err(), "validate of missing session must fail");

    // admin adds p1, then transitions Spawned → Working. Both succeed.
    admin
        .run(
            "agent.add.execute",
            json!({"id": "p1", "rig": "granite"}),
            AgentCommand::Add(add_args),
            false,
        )
        .await
        .expect("admin add.execute should succeed");

    admin
        .run(
            "agent.transition.execute",
            json!({"id": "p1", "to": "working"}),
            AgentCommand::Transition(TransitionSession {
                id: "p1".into(),
                to: SessionState::Working,
            }),
            false,
        )
        .await
        .expect("admin transition Spawned -> Working should succeed");

    // Illegal transition Working → Spawned must be rejected by the state machine.
    let illegal = admin
        .run(
            "agent.transition.execute",
            json!({"id": "p1", "to": "spawned"}),
            AgentCommand::Transition(TransitionSession {
                id: "p1".into(),
                to: SessionState::Spawned,
            }),
            false,
        )
        .await;
    let illegal_msg = illegal.expect_err("illegal transition must fail").message.to_string();
    assert!(
        illegal_msg.contains("invalid state transition"),
        "illegal msg: {illegal_msg}",
    );

    // Actor advanced exactly once: p1 is Working.
    let sessions = agent.snapshot().await;
    assert_eq!(sessions.len(), 1, "expected one session, got {sessions:?}");
    let s = &sessions[0];
    assert_eq!(s.id, "p1");
    assert_eq!(s.rig, "granite");
    assert_eq!(s.state, SessionState::Working);

    let watcher_events = watcher_audit.snapshot();
    assert!(
        watcher_events.iter().any(|e| matches!(
            e,
            AuditEvent::Unauthorized { tool, .. } if tool == "agent.add.execute"
        )),
        "watcher audit missing Unauthorized for add.execute: {watcher_events:?}",
    );
    assert!(
        watcher_events.iter().any(|e| matches!(
            e,
            AuditEvent::Invoked { tool, outcome: Outcome::Failed { .. }, .. } if tool == "agent.transition.validate"
        )),
        "watcher audit missing failed validate: {watcher_events:?}",
    );

    let admin_events = admin_audit.snapshot();
    assert!(
        admin_events.iter().any(|e| matches!(
            e,
            AuditEvent::Invoked { tool, outcome: Outcome::Ok, .. } if tool == "agent.add.execute"
        )),
        "admin audit missing ok add.execute: {admin_events:?}",
    );
    assert!(
        admin_events.iter().any(|e| matches!(
            e,
            AuditEvent::Invoked { tool, outcome: Outcome::Ok, .. } if tool == "agent.transition.execute"
        )),
        "admin audit missing ok transition.execute: {admin_events:?}",
    );
    assert!(
        admin_events.iter().any(|e| matches!(
            e,
            AuditEvent::Invoked { tool, outcome: Outcome::Failed { .. }, .. } if tool == "agent.transition.execute"
        )),
        "admin audit missing failed illegal transition.execute: {admin_events:?}",
    );
}
