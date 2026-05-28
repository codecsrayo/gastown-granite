//! `gt-mcp` binary entry. Boots the composition root (`bins/gt`) and serves the MCP
//! service over stdio, sharing the root's `AgentHandle` so MCP tool calls drive the same
//! session actor the root drives — not an isolated copy (Paso 6.f.3). Every invocation is
//! persisted to the shared events.jsonl through the gt-audit writer (Paso 6.f.4).

use std::sync::Arc;

use rmcp::transport::stdio;
use rmcp::ServiceExt;

use gt_audit::JsonlWriter;
use gt_beads::InMemoryBeads;
use gt_root::{spawn, LogEffects, RootConfig, SystemClock};

use gt_mcp::{audit::AuditSink, auth::Scope, JsonlAudit, McpService};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let log_path =
        std::env::var("GT_EVENT_LOG").unwrap_or_else(|_| "/tmp/gt.events.jsonl".to_string());

    // In-memory adapters keep the bin runnable end-to-end before the Dolt-backed read-side
    // lands — same stand-in gt-web uses. Real adapters slot in here without touching the MCP
    // service.
    let beads = Arc::new(InMemoryBeads::default());
    let root = spawn(beads, LogEffects, SystemClock, &log_path, RootConfig::default());

    // Frontier audit lands in the same event log as the domain events. A second JsonlWriter on
    // the same path is safe: each append takes the file lock, so lines never interleave. The
    // `mcp.` prefix marks these as observability — domain replay skips it (`replay_gt`).
    let audit: Arc<dyn AuditSink> =
        Arc::new(JsonlAudit::new(Arc::new(JsonlWriter::new(&log_path))));
    let scope =
        Scope::admin(std::env::var("GT_MCP_ACTOR").unwrap_or_else(|_| "mcp-local".to_string()));

    // Share the root's domain actors, not isolated `actor::spawn`s. MCP tool calls drive the
    // same agent + merge + scheduling + patrol + orchestration actors the root drives, so their
    // events land in the shared log.
    let service = McpService::new(
        root.agent.clone(),
        root.merge.clone(),
        root.sched.clone(),
        root.patrol.clone(),
        root.orch.clone(),
        scope,
        audit,
    );

    let handle = service.serve(stdio()).await?;
    handle.waiting().await?;

    root.shutdown();
    Ok(())
}
