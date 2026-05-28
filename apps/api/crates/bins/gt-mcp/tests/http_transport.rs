//! Gate test for Paso 6.f.12: gt-mcp serves over streamable HTTP, end-to-end.
//!
//! Mounts [`gt_mcp::http::router`] on an ephemeral port, then drives it with a real `rmcp`
//! streamable-HTTP client: initialize handshake + `tools/list` over HTTP. Asserts the tool
//! registry the stdio transport exposes is reachable over HTTP too — proving the alternate
//! transport carries the same `McpService` (Paso 6.f.12; stdio stays the default).

use std::sync::Arc;

use rmcp::transport::StreamableHttpClientTransport;
use rmcp::ServiceExt;

use gt_mcp::{
    audit::{AuditSink, InMemoryAudit},
    auth::Scope,
    McpService,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_transport_serves_tool_list_over_mcp() {
    let audit: Arc<dyn AuditSink> = Arc::new(InMemoryAudit::new());
    let service = McpService::new(
        actor_agent(),
        actor_merge(),
        actor_sched(),
        actor_patrol(),
        actor_orch(),
        actor_quota(),
        Scope::admin("http-test"),
        audit,
    );

    // Bind an ephemeral port, then serve the MCP HTTP router on it.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, gt_mcp::http::router(service)).await.unwrap();
    });

    // Real rmcp streamable-HTTP client: connect + initialize + list tools.
    let transport = StreamableHttpClientTransport::from_uri(format!("http://{addr}/mcp"));
    let client = ()
        .serve(transport)
        .await
        .expect("client should connect + complete the MCP initialize handshake over HTTP");

    let tools = client.list_all_tools().await.expect("tools/list over HTTP");
    let names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();

    // One execute tool from every retrofitted domain must be reachable over HTTP.
    for expected in [
        "agent.add.execute",
        "merge.submit.execute",
        "scheduling.enqueue.execute",
        "patrol.register.execute",
        "orch.launch_convoy.execute",
        "quota.sample.execute",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "tool {expected} not listed over HTTP transport: {names:?}",
        );
    }

    // Both .validate and .execute variants are published (the tool-pair contract of doc 09).
    assert!(
        names.iter().any(|n| n == "agent.add.validate"),
        "validate variant missing: {names:?}",
    );

    let _ = client.cancel().await;
    server.abort();
}

fn actor_agent() -> gt_agent::actor::AgentHandle {
    gt_agent::actor::spawn(16)
}

fn actor_merge() -> gt_merge::actor::MergeHandle {
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    gt_merge::actor::spawn(tx)
}

fn actor_sched() -> gt_scheduling::actor::SchedHandle {
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    gt_scheduling::actor::spawn(Arc::new(gt_beads::InMemoryBeads::default()), tx, 4)
}

fn actor_patrol() -> gt_patrol::actor::PatrolHandle {
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    gt_patrol::actor::spawn(tx)
}

fn actor_orch() -> gt_orchestration::actor::OrchHandle {
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    gt_orchestration::actor::spawn(tx)
}

fn actor_quota() -> gt_quota::actor::QuotaHandle {
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    gt_quota::actor::spawn(tx, std::collections::HashMap::new())
}
