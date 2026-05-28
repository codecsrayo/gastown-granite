//! Gate test for Paso 6.f.1: gt-mcp drives the agent domain end-to-end through the
//! `Command { validate, execute }` path.
//!
//! Covers:
//! - `tools/list` reports `.validate` + `.execute` variants for every command.
//! - A `read_only` scope rejects `.execute` and records `Unauthorized` audit.
//! - A `validate` call against a session that does not exist returns an error but
//!   leaves no state change (and is still audited as `Invoked { Failed }`).
//! - An `execute` call that transitions `Spawned → Working` succeeds and is audited
//!   as `Invoked { Ok }`.
//! - An illegal transition (`Working → Spawned`) is rejected by the same dispatcher
//!   and shows up as `Invoked { Failed }` — the state machine guarded by the actor.

use std::sync::Arc;

use serde_json::{json, Value};

use gt_agent::actor;
use gt_mcp::{
    audit::{AuditEvent, AuditSink, InMemoryAudit, Outcome},
    auth::Scope,
    server::{make_request, Dispatcher},
    tools::ToolRegistry,
};

fn ok_result(resp: &gt_mcp::server::Response) -> &Value {
    assert!(
        resp.error.is_none(),
        "expected ok, got error: {:?}",
        resp.error.as_ref().map(|e| &e.message)
    );
    resp.result.as_ref().expect("missing result")
}

fn err_message(resp: &gt_mcp::server::Response) -> &str {
    &resp.error.as_ref().expect("expected error").message
}

fn call(name: &str, arguments: Value) -> gt_mcp::server::Request {
    make_request(json!(name), "tools/call", json!({ "name": name, "arguments": arguments }))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_gate_validate_only_blocks_execute_full_scope_drives_state() {
    // --- arrange ---
    let agent = actor::spawn(16);
    let registry = ToolRegistry::new(agent.clone());

    // 1) Validate-only scope: must reject `.execute` and only `.execute`.
    let watcher_audit = Arc::new(InMemoryAudit::new());
    let watcher = Dispatcher::new(
        registry.clone(),
        Scope::read_only("watcher"),
        Arc::clone(&watcher_audit) as Arc<dyn AuditSink>,
    );

    // 2) Admin scope: drives the actor.
    let admin_audit = Arc::new(InMemoryAudit::new());
    let admin = Dispatcher::new(
        registry,
        Scope::admin("admin"),
        Arc::clone(&admin_audit) as Arc<dyn AuditSink>,
    );

    // --- act / assert ---

    // tools/list reports both variants for every command.
    let listed = watcher
        .handle(make_request(json!(1), "tools/list", json!({})))
        .await;
    let tools = ok_result(&listed)
        .get("tools")
        .expect("tools/list missing tools")
        .as_array()
        .expect("tools is not array");
    let names: Vec<String> = tools
        .iter()
        .map(|t| t.get("name").unwrap().as_str().unwrap().to_string())
        .collect();
    for base in ["agent.add", "agent.remove", "agent.transition"] {
        assert!(names.contains(&format!("{base}.validate")), "missing {base}.validate");
        assert!(names.contains(&format!("{base}.execute")), "missing {base}.execute");
    }

    // watcher tries to add a session → blocked by scope, audited as Unauthorized.
    let denied = watcher
        .handle(call("agent.add.execute", json!({"id": "p1", "rig": "granite"})))
        .await;
    assert!(denied.error.is_some(), "execute on read_only should fail");
    assert!(err_message(&denied).contains("validate_only"));

    // watcher can still validate something that does not exist → audit Invoked Failed.
    let validate_missing = watcher
        .handle(call("agent.transition.validate", json!({"id": "ghost", "to": "working"})))
        .await;
    assert!(validate_missing.error.is_some(), "validate of missing session should fail");

    // admin adds p1, then transitions Spawned → Working. Both succeed.
    let added = admin
        .handle(call("agent.add.execute", json!({"id": "p1", "rig": "granite"})))
        .await;
    ok_result(&added);

    let transitioned = admin
        .handle(call("agent.transition.execute", json!({"id": "p1", "to": "working"})))
        .await;
    ok_result(&transitioned);

    // Illegal transition Working → Spawned must be rejected, but recorded as Invoked Failed.
    let illegal = admin
        .handle(call("agent.transition.execute", json!({"id": "p1", "to": "spawned"})))
        .await;
    assert!(illegal.error.is_some(), "illegal transition must fail");
    assert!(err_message(&illegal).contains("invalid state transition"));

    // Snapshot — actor state advanced exactly once: p1 is Working.
    let sessions = agent.snapshot().await;
    assert_eq!(sessions.len(), 1, "expected one session, got {sessions:?}");
    let s = &sessions[0];
    assert_eq!(s.id, "p1");
    assert_eq!(s.rig, "granite");
    assert_eq!(s.state, gt_agent::SessionState::Working);

    // --- audit assertions ---
    let watcher_events = watcher_audit.snapshot();
    assert!(
        watcher_events
            .iter()
            .any(|e| matches!(e, AuditEvent::Unauthorized { tool, .. } if tool == "agent.add.execute")),
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
