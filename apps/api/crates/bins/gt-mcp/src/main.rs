//! `gt-mcp` binary entry. Spawns the agent actor, builds a single-actor scope, wires
//! an in-memory audit (placeholder until `gt-audit` is plugged in by the composition
//! root), and drives the stdio JSON-RPC loop.
//!
//! This is the smallest viable seam from `docs/09-llm-integration.md` Paso 6.f.1:
//! one domain, three tools, two variants each. Wiring more domains is additive — add
//! the registry adapter and the schema, no changes here.

use std::sync::Arc;

use gt_agent::actor;
use gt_mcp::{
    audit::AuditSink, auth::Scope, server::serve_stdio, server::Dispatcher,
    tools::ToolRegistry, InMemoryAudit,
};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let agent = actor::spawn(64);
    let registry = ToolRegistry::new(agent);
    let audit: Arc<dyn AuditSink> = Arc::new(InMemoryAudit::new());
    let scope = Scope::admin(
        std::env::var("GT_MCP_ACTOR").unwrap_or_else(|_| "mcp-local".to_string()),
    );
    let dispatcher = Dispatcher::new(registry, scope, audit);
    serve_stdio(dispatcher).await
}
