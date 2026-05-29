//! Prometheus `/metrics` endpoint for the `gt` orchestrator.
//!
//! `gt` is the only bin that calls [`gt_telemetry::record_envelope`] today, so it owns the
//! `gt_events_total` / `gt_dead_letter_total` counters that have real samples. Without an
//! HTTP exposition endpoint the registry stays trapped in the process: gt-web exposes
//! `/metrics` but its registry is empty.
//!
//! This module spins a minimal Axum server bound to `GT_METRICS_BIND` (default
//! `0.0.0.0:9100`). It reuses the same process-global registry as gt-web — no separate
//! registry, no double counting.

use axum::routing::get;
use axum::Router;

const DEFAULT_BIND: &str = "0.0.0.0:9100";

pub fn bind_addr() -> String {
    std::env::var("GT_METRICS_BIND").unwrap_or_else(|_| DEFAULT_BIND.to_string())
}

async fn metrics_handler() -> Result<String, (axum::http::StatusCode, String)> {
    gt_telemetry::metrics::render_text()
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

async fn health_handler() -> &'static str {
    "ok"
}

pub fn router() -> Router {
    Router::new()
        .route("/metrics", get(metrics_handler))
        .route("/health", get(health_handler))
}

pub async fn serve(addr: &str) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!(
        "[gt] metrics endpoint listening on http://{}/metrics",
        listener.local_addr()?
    );
    axum::serve(listener, router())
        .await
        .map_err(std::io::Error::other)
}
