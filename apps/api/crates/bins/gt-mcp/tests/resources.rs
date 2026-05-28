//! Gate test for Paso 6.f.11: the read-side domain snapshots are exposed as MCP Resources.
//!
//! Drives the testable plain methods (`resource_list` / `read_resource_json`) that the rmcp
//! `ServerHandler::{list_resources,read_resource}` delegate to — same code path the wire hits,
//! without standing up a RequestContext.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::json;

use gt_agent::actor as agent_actor;
use gt_agent::{AddSession, AgentCommand};
use gt_mcp::{
    audit::{AuditSink, InMemoryAudit},
    auth::Scope,
    McpService,
};
use tokio::sync::mpsc;

fn full_service(scope: Scope) -> McpService {
    let agent = agent_actor::spawn(16);
    let (merge_tx, _merge_rx) = mpsc::channel(16);
    let merge = gt_merge::actor::spawn(gt_merge::InMemoryMergeRepo::default(), merge_tx);
    let (sched_tx, _sched_rx) = mpsc::channel(16);
    let sched =
        gt_scheduling::actor::spawn(Arc::new(gt_beads::InMemoryBeads::default()), sched_tx, 4);
    let (patrol_tx, _patrol_rx) = mpsc::channel(16);
    let patrol = gt_patrol::actor::spawn(gt_patrol::InMemoryPatrolRepo::default(), patrol_tx);
    let (orch_tx, _orch_rx) = mpsc::channel(16);
    let orch = gt_orchestration::actor::spawn(gt_orchestration::InMemoryOrchRepo::default(), orch_tx);
    let (quota_tx, _quota_rx) = mpsc::channel(16);
    let quota = gt_quota::actor::spawn(quota_tx, HashMap::new());
    let audit: Arc<dyn AuditSink> = Arc::new(InMemoryAudit::new());
    McpService::new(agent, merge, sched, patrol, orch, quota, scope, audit)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resource_catalog_lists_every_domain_snapshot() {
    let svc = full_service(Scope::admin("max"));
    let uris: Vec<String> = svc.resource_list().into_iter().map(|r| r.uri).collect();
    assert_eq!(
        uris,
        vec![
            "gt://agent/sessions",
            "gt://scheduling/queue",
            "gt://patrol/leases",
            "gt://merge/slots",
            "gt://orch/convoys",
            "gt://quota/accounts",
        ],
    );
    // Every catalog entry must be readable.
    for uri in &uris {
        svc.read_resource_json(uri).await.unwrap_or_else(|e| panic!("read {uri}: {e}"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_sessions_resource_reflects_state() {
    let svc = full_service(Scope::admin("max"));

    // Empty to start.
    let before = svc.read_resource_json("gt://agent/sessions").await.unwrap();
    assert_eq!(before, json!([]), "no sessions yet");

    // Add a session via the tool path, then the resource reflects it.
    svc.run(
        "agent.add.execute",
        json!({"id": "p1", "rig": "granite"}),
        AgentCommand::Add(AddSession { id: "p1".into(), rig: "granite".into() }),
        false,
    )
    .await
    .expect("add.execute should succeed");

    let after = svc.read_resource_json("gt://agent/sessions").await.unwrap();
    let arr = after.as_array().expect("sessions is a JSON array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"], "p1");
    assert_eq!(arr[0]["rig"], "granite");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn counter_resources_and_unknown_uri() {
    let svc = full_service(Scope::admin("max"));

    let sched = svc.read_resource_json("gt://scheduling/queue").await.unwrap();
    assert_eq!(sched["queued"], 0);
    assert_eq!(sched["in_flight"], 0);

    let patrol = svc.read_resource_json("gt://patrol/leases").await.unwrap();
    assert!(patrol.get("live_leases").is_some());
    assert!(patrol.get("expired_emitted").is_some());

    let quota = svc.read_resource_json("gt://quota/accounts").await.unwrap();
    assert!(quota.get("accounts").is_some());

    // merge/orch read as empty arrays before any activity.
    assert_eq!(svc.read_resource_json("gt://merge/slots").await.unwrap(), json!([]));
    assert_eq!(svc.read_resource_json("gt://orch/convoys").await.unwrap(), json!([]));

    // Unknown uri is an error, not an empty read.
    assert!(svc.read_resource_json("gt://nope").await.is_err());
}
