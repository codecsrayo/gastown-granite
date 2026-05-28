//! `gt-web` — backend-only API + SSE for the browser frontend (`docs/07-frontend.md`).
//!
//! What this crate is:
//! - The read-side composition root: cables `gt-agent::SessionQueries`, `gt-beads::BeadRepository`
//!   and the running root's event broadcast into an Axum router.
//! - A thin gateway: snapshot endpoints (REST) + a delta stream (SSE) + one write command
//!   (`/api/nudge`). DTOs translate domain types to a stable JSON contract.
//! - The IAM frontier: a bearer-token middleware (`auth`) wraps every route, and every
//!   accepted/rejected request is recorded in the shared event log via [`audit`] (`web.*`
//!   frontier-audit records, same shape as `gt-mcp`'s `mcp.*`).
//!
//! What this crate is **not**: the dashboard UI. The browser app lives in `apps/town/` under
//! its own SvelteKit plan and is not migrated to Rust. We only ship the contract it consumes.

pub mod audit;
pub mod auth;
pub mod dto;
pub mod routes;
pub mod state;
pub mod stream;

use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;

use gt_agent::SessionQueries;
use gt_beads::BeadRepository;

pub use audit::{InMemoryWebAudit, JsonlWebAudit, WebAuditEvent, WebAuditSink};
pub use auth::{AuthConfig, AuthLayer};
pub use state::AppState;

/// Build the router around an [`AppState`]. The auth middleware runs first (`from_fn_with_state`)
/// and short-circuits unauthorized requests with `401` before any handler sees them. The same
/// builder is used by `main.rs` and the gate tests.
pub fn router<R, SQ>(
    state: AppState<R, SQ>,
    auth: AuthConfig,
    audit: Arc<dyn WebAuditSink>,
) -> Router
where
    R: BeadRepository + Send + Sync + 'static,
    SQ: SessionQueries + Send + Sync + 'static,
{
    let layer = AuthLayer { config: auth, audit };
    Router::new()
        .route("/api/sessions", get(routes::list_sessions::<R, SQ>))
        .route("/api/beads", get(routes::list_beads::<R, SQ>))
        .route("/api/nudge", post(routes::nudge::<R, SQ>))
        .route("/api/stream", get(routes::stream::<R, SQ>))
        .route("/metrics", get(routes::metrics))
        .with_state(state)
        .layer(axum::middleware::from_fn_with_state(
            layer,
            auth::auth_middleware,
        ))
}
