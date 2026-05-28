//! `gt-mcp` binary entry. Spawns the agent actor, builds a scope from env, attaches
//! an in-memory audit (placeholder until `gt-audit` is wired by the composition
//! root), and serves the MCP service over stdio via the official `rmcp` SDK.

use std::sync::Arc;

use rmcp::transport::stdio;
use rmcp::ServiceExt;

use gt_agent::actor;
use gt_mcp::{audit::AuditSink, auth::Scope, InMemoryAudit, McpService};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let agent = actor::spawn(64);
    let audit: Arc<dyn AuditSink> = Arc::new(InMemoryAudit::new());
    let scope = Scope::admin(
        std::env::var("GT_MCP_ACTOR").unwrap_or_else(|_| "mcp-local".to_string()),
    );
    let service = McpService::new(agent, scope, audit);
    let handle = service.serve(stdio()).await?;
    handle.waiting().await?;
    Ok(())
}
