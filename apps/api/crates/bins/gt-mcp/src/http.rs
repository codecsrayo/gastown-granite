//! Streamable-HTTP transport for `gt-mcp` (Paso 6.f.12).
//!
//! `main.rs` mounts the [`McpService`] over stdio by default; this module is the alternate
//! transport — the official `rmcp` streamable-HTTP server wrapped in an Axum router. Both
//! transports share the *same* [`McpService`] (and thus the same root actor handles), so an
//! HTTP client drives the exact dispatch + scope + audit path the stdio tool router uses.
//!
//! The session manager is the in-process [`LocalSessionManager`]: each MCP session gets a
//! clone of the service (cheap — `McpService` is `Arc`-backed), so concurrent HTTP clients
//! all reach the one set of domain actors.

use std::sync::Arc;

use axum::routing::get;
use axum::Router;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};

use crate::McpService;

/// Path the MCP endpoint is mounted at.
pub const MCP_PATH: &str = "/mcp";

/// Build the Axum router exposing `service` over streamable HTTP at [`MCP_PATH`]. The service
/// factory hands every session a clone of the shared `McpService`. The same listener also
/// serves `/metrics` (Prometheus text exposition) and `/health`. gt-mcp does not call
/// `record_envelope` yet (only `gt` does), so the counters are empty here today; the route
/// is in place so future instrumentation lights up without a re-deploy.
pub fn router(service: McpService) -> Router {
    let http = StreamableHttpService::new(
        move || Ok(service.clone()),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    Router::new()
        .nest_service(MCP_PATH, http)
        .route("/metrics", get(metrics_handler))
        .route("/health", get(|| async { "ok" }))
}

async fn metrics_handler() -> Result<String, (axum::http::StatusCode, String)> {
    gt_telemetry::metrics::render_text()
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// Bind `addr` and serve the MCP HTTP transport until the listener closes.
pub async fn serve(service: McpService, addr: &str) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!(
        "[gt-mcp] http transport listening on http://{}{MCP_PATH}",
        listener.local_addr()?
    );
    axum::serve(listener, router(service)).await?;
    Ok(())
}
