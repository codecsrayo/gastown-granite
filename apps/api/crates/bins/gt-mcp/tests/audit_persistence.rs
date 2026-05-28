//! Gate test for Paso 6.f.4: the `JsonlAudit` sink persists every MCP invocation to the
//! gt-audit event log as a type-erased `EventRecord`, and those `mcp.*` meta records do not
//! break domain replay (`gt_root::replay_gt` skips the meta prefix).
//!
//! Drives `McpService::run` directly — the shared dispatch path behind every generated
//! `#[tool]` — with a `JsonlAudit` backed by a real `JsonlWriter` on a temp file, then reads
//! the log back and asserts the invocation + the scope rejection both landed.

use std::sync::Arc;

use serde_json::json;

use gt_agent::actor;
use gt_agent::{AddSession, AgentCommand};
use gt_audit::{read_all, JsonlWriter};
use gt_mcp::{audit::AuditSink, auth::Scope, JsonlAudit, McpService};

fn temp_log() -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "gt-mcp-audit-{}-{}.events.jsonl",
        std::process::id(),
        nanos
    ))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jsonl_audit_persists_invocations_to_log_and_replay_tolerates_them() {
    let log_path = temp_log();
    let _ = std::fs::remove_file(&log_path);

    let agent = actor::spawn(16);
    // Agent-only gate; wire a merge actor so the service has its full surface (relay receiver
    // kept alive so the actor's sends don't error).
    let (merge_tx, _merge_rx) = tokio::sync::mpsc::channel(16);
    let merge = gt_merge::actor::spawn(merge_tx);
    let audit: Arc<dyn AuditSink> =
        Arc::new(JsonlAudit::new(Arc::new(JsonlWriter::new(&log_path))));

    let admin = McpService::new(agent.clone(), merge.clone(), Scope::admin("max"), Arc::clone(&audit));
    let watcher = McpService::new(agent.clone(), merge.clone(), Scope::read_only("watcher"), Arc::clone(&audit));

    // admin executes a real add → Invoked { Ok }, persisted.
    let add = AddSession { id: "p1".into(), rig: "granite".into() };
    admin
        .run(
            "agent.add.execute",
            json!({"id": "p1", "rig": "granite"}),
            AgentCommand::Add(add),
            false,
        )
        .await
        .expect("admin add.execute should succeed");

    // watcher (validate_only) is rejected by scope → Unauthorized, persisted.
    let denied = watcher
        .run(
            "agent.add.execute",
            json!({"id": "p2", "rig": "granite"}),
            AgentCommand::Add(AddSession { id: "p2".into(), rig: "granite".into() }),
            false,
        )
        .await;
    assert!(denied.is_err(), "execute on read_only scope must be rejected");

    // --- both invocations are in the log, type-erased ----------------------------------
    let records = read_all(&log_path).expect("read audit log");
    assert_eq!(records.len(), 2, "two invocations persisted, got {records:?}");

    let invoked = records
        .iter()
        .find(|r| r.kind == "mcp.invoked")
        .expect("mcp.invoked record present");
    assert_eq!(invoked.payload["actor"], "max");
    assert_eq!(invoked.payload["tool"], "agent.add.execute");
    assert_eq!(invoked.payload["outcome"]["status"], "ok");
    assert!(!invoked.event_id.is_empty(), "edge stamps an event id");

    let unauthorized = records
        .iter()
        .find(|r| r.kind == "mcp.unauthorized")
        .expect("mcp.unauthorized record present");
    assert_eq!(unauthorized.payload["actor"], "watcher");
    assert_eq!(unauthorized.payload["tool"], "agent.add.execute");

    // --- replay tolerates a meta-only log: domain state stays empty, no error ----------
    let state = gt_root::replay_gt(&records).expect("replay must not choke on mcp.* records");
    assert!(
        state.agent.snapshot().is_empty(),
        "mcp.* audit events carry no domain state, agent registry stays empty",
    );
    assert_eq!(
        state.feed.total_events as usize,
        records.len(),
        "feed folds every record, meta included",
    );

    let _ = std::fs::remove_file(&log_path);
}
