//! `gt-mcp` binary entry. Boots the composition root (`bins/gt`) and serves the MCP
//! service over stdio, sharing the root's `AgentHandle` so MCP tool calls drive the same
//! session actor the root drives — not an isolated copy (Paso 6.f.3). The audit sink stays
//! in-memory until the gt-audit writer is wired (Paso 6.f.4).

use std::sync::Arc;

use rmcp::transport::stdio;
use rmcp::ServiceExt;

use gt_beads::InMemoryBeads;
use gt_root::{spawn, LogEffects, RootConfig, SystemClock};

use gt_mcp::{audit::AuditSink, auth::Scope, InMemoryAudit, McpService};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let log_path =
        std::env::var("GT_EVENT_LOG").unwrap_or_else(|_| "/tmp/gt.events.jsonl".to_string());

    // In-memory adapters keep the bin runnable end-to-end before the Dolt-backed read-side
    // lands — same stand-in gt-web uses. Real adapters slot in here without touching the MCP
    // service.
    let beads = Arc::new(InMemoryBeads::default());
    let root = spawn(beads, LogEffects, SystemClock, &log_path, RootConfig::default());

    let audit: Arc<dyn AuditSink> = Arc::new(InMemoryAudit::new());
    let scope =
        Scope::admin(std::env::var("GT_MCP_ACTOR").unwrap_or_else(|_| "mcp-local".to_string()));

    // Share the root's agent actor, not an isolated `actor::spawn`.
    let service = McpService::new(root.agent.clone(), scope, audit);

    let handle = service.serve(stdio()).await?;
    handle.waiting().await?;

    root.shutdown();
    Ok(())
}
