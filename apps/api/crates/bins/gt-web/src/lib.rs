//! `gt-web` — backend-only API + SSE for the browser frontend (`docs/07-frontend.md`).
//!
//! What this crate is:
//! - The read-side composition root: cables `gt-agent::SessionQueries`, `gt-beads::BeadRepository`
//!   and the running root's event broadcast into an Axum router.
//! - A thin gateway: snapshot endpoints (REST) + a delta stream (SSE) + one write command
//!   (`/api/nudge`). DTOs translate domain types to a stable JSON contract.
//!
//! What this crate is **not**: the dashboard UI. The browser app lives in `apps/town/` under
//! its own SvelteKit plan and is not migrated to Rust. We only ship the contract it consumes.

pub mod dto;
pub mod routes;
pub mod state;
pub mod stream;

use axum::routing::{get, post};
use axum::Router;

use gt_agent::SessionQueries;
use gt_beads::BeadRepository;

pub use state::AppState;

/// Build the router around an [`AppState`]. The same builder is used by `main.rs` and by the
/// gate test (which binds it onto a transient socket).
pub fn router<R, SQ>(state: AppState<R, SQ>) -> Router
where
    R: BeadRepository + Send + Sync + 'static,
    SQ: SessionQueries + Send + Sync + 'static,
{
    Router::new()
        .route("/api/sessions", get(routes::list_sessions::<R, SQ>))
        .route("/api/beads", get(routes::list_beads::<R, SQ>))
        .route("/api/nudge", post(routes::nudge::<R, SQ>))
        .route("/api/stream", get(routes::stream::<R, SQ>))
        .with_state(state)
}
