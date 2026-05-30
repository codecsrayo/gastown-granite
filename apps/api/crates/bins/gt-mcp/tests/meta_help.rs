//! Gate test for `hq-mcp-onboard.7`: `meta.help` tool returns server version, the full
//! tool index (names + descriptions) and the resource catalog in one call.
//!
//! Drives the plain helper `meta_help_payload()` — same shape the wire serves via the
//! `#[tool(name = "meta.help")]` dispatch, without standing up a `RequestContext`.

use std::collections::HashMap;
use std::sync::Arc;

use gt_mcp::{
    audit::{AuditSink, InMemoryAudit},
    auth::Scope,
    McpService,
};
use tokio::sync::mpsc;

fn full_service(scope: Scope) -> McpService {
    let agent = gt_agent::actor::spawn(16);
    let (merge_tx, _merge_rx) = mpsc::channel(16);
    let merge = gt_merge::actor::spawn(gt_merge::InMemoryMergeRepo::default(), merge_tx);
    let (sched_tx, _sched_rx) = mpsc::channel(16);
    let sched = gt_scheduling::actor::spawn(
        Arc::new(gt_beads::InMemoryBeads::default()),
        sched_tx,
        4,
    );
    let (patrol_tx, _patrol_rx) = mpsc::channel(16);
    let patrol = gt_patrol::actor::spawn(gt_patrol::InMemoryPatrolRepo::default(), patrol_tx);
    let (orch_tx, _orch_rx) = mpsc::channel(16);
    let orch = gt_orchestration::actor::spawn(
        gt_orchestration::InMemoryOrchRepo::default(),
        orch_tx,
    );
    let (quota_tx, _quota_rx) = mpsc::channel(16);
    let quota = gt_quota::actor::spawn(quota_tx, HashMap::new());
    let audit: Arc<dyn AuditSink> = Arc::new(InMemoryAudit::new());
    McpService::new(agent, merge, sched, patrol, orch, quota, scope, audit)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn meta_help_payload_includes_server_tools_and_resources() {
    let svc = full_service(Scope::admin("max"));
    let payload = svc.meta_help_payload();

    let server = payload.get("server").expect("server key");
    assert_eq!(
        server.get("name").and_then(|v| v.as_str()),
        Some("gt-mcp"),
        "server.name should be the crate name"
    );
    assert!(
        server
            .get("version")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty()),
        "server.version must be a non-empty semver string"
    );

    let tools = payload
        .get("tools")
        .and_then(|v| v.as_array())
        .expect("tools array");
    assert!(
        tools.len() >= 20,
        "expected at least ~20 tools in the index, got {}",
        tools.len()
    );
    let tool_names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(|v| v.as_str()))
        .collect();
    for required in &[
        "meta.help",
        "agent.add.validate",
        "agent.add.execute",
        "scheduling.create_bead.execute",
        "patrol.register.execute",
    ] {
        assert!(
            tool_names.contains(required),
            "tool index missing `{required}` — got names: {tool_names:?}"
        );
    }

    let resources = payload
        .get("resources")
        .and_then(|v| v.as_array())
        .expect("resources array");
    let uris: Vec<&str> = resources
        .iter()
        .filter_map(|r| r.get("uri").and_then(|v| v.as_str()))
        .collect();
    for required in &[
        "gt://agent/sessions",
        "gt://scheduling/queue",
        "gt://issues",
    ] {
        assert!(
            uris.contains(required),
            "resource catalog missing `{required}` — got uris: {uris:?}"
        );
    }
}
