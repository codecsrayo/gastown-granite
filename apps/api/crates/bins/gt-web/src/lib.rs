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
pub mod health;
pub mod idempotency;
pub mod control;
pub mod routes;
pub mod state;
pub mod stream;

use std::sync::Arc;

use axum::routing::{delete, get, patch, post};
use axum::Router;

use gt_agent::SessionQueries;
use gt_beads::BeadRepository;

pub use audit::{InMemoryWebAudit, JsonlWebAudit, WebAuditEvent, WebAuditSink};
pub use auth::{AuthConfig, AuthLayer};
pub use health::{HydrationHandle, ReadinessGate, ReadinessGateBuilder};
pub use idempotency::{idempotency_middleware, IdempotencyStore};
pub use control::{
    InMemoryPolecatControl, InMemoryPolecatRespawner, LifecyclePolecatRespawner, PolecatControl,
    PolecatRespawner, RespawnInfo, TmuxPolecatControl,
};
pub use routes::collect_worktrees;
pub use state::AppState;

/// Build the router around an [`AppState`].
///
/// The router is composed of two sub-routers merged at `/`:
///
/// - `api` — every `/api/*` route plus the SSE stream, wrapped by the bearer-token
///   [`auth::auth_middleware`]. Unauthorized requests short-circuit with `401` before any
///   handler sees them.
/// - `probes` — `/health`, `/readyz` and `/metrics`. Operators (systemd, kube, Prometheus)
///   probe these without an `Authorization` header, so they sit **outside** the auth layer
///   on purpose (paso 8.5, hq-8iur.5). `/metrics` previously lived behind auth; bringing it
///   out aligns with how Prometheus scrapes.
pub fn router<R, SQ>(
    state: AppState<R, SQ>,
    auth: AuthConfig,
    audit: Arc<dyn WebAuditSink>,
    readiness: ReadinessGate,
) -> Router
where
    R: BeadRepository + Send + Sync + 'static,
    SQ: SessionQueries + Send + Sync + 'static,
{
    router_with_idempotency(state, auth, audit, readiness, IdempotencyStore::with_defaults())
}

/// `router` variant that lets the caller inject a pre-built idempotency store. Production
/// boot in `main.rs` uses [`router`]; tests reuse this so they can run with a tighter TTL
/// or assert directly on the cache state.
pub fn router_with_idempotency<R, SQ>(
    state: AppState<R, SQ>,
    auth: AuthConfig,
    audit: Arc<dyn WebAuditSink>,
    readiness: ReadinessGate,
    idempotency: IdempotencyStore,
) -> Router
where
    R: BeadRepository + Send + Sync + 'static,
    SQ: SessionQueries + Send + Sync + 'static,
{
    let layer = AuthLayer { config: auth, audit };
    let api = Router::new()
        .route("/api/sessions", get(routes::list_sessions::<R, SQ>))
        // hq-fe-api-w.6 — operator e-stop on a runaway polecat. Tmux-side kill goes
        // through `AppState.control` (a thin port over `gt_polecat::Tmux`); the registry
        // close happens via the existing `AgentEvent::Killed` projector path so SSE
        // subscribers see the same `agent.killed` record the in-process reactor produces.
        .route(
            "/api/sessions/:id",
            delete(routes::delete_session::<R, SQ>),
        )
        // hq-fe-api-w.8 — softer e-stop: `tmux send-keys Escape` cancels the agent's
        // in-flight turn without killing the polecat. The same `PolecatControl` port
        // handles both ops so the production wiring stays a single shared `TmuxCli`.
        .route(
            "/api/sessions/:id/interrupt",
            post(routes::interrupt_session::<R, SQ>),
        )
        // hq-fe-api-w.7 — cold-restart: respawn a stuck polecat with the same hook
        // bead + convoy by reading env from the dying session before tearing it down.
        // Emits the close+reopen `AgentEvent` pair the supervisor would on a real
        // restart so SSE subscribers + projector see a single atomic transition.
        .route(
            "/api/sessions/:id/restart",
            post(routes::restart_session::<R, SQ>),
        )
        // hq-fe-api-w.3: write surface on the dispatcher's bead table — POST mints a
        // `pending` row (wrapping scheduling.create_bead), PATCH partially updates
        // title/priority/assignee. Status transitions stay on the reactor.
        .route(
            "/api/beads",
            get(routes::list_beads::<R, SQ>).post(routes::create_bead::<R, SQ>),
        )
        .route("/api/beads/:id", patch(routes::update_bead::<R, SQ>))
        // hq-fe-api-w.4 — operator override for the bead state machine. Not a reactor:
        // dispatcher capacity stays unchanged so a real worker's lifecycle is not double-
        // counted. See [`routes::transition_bead`] for the allowed transition matrix.
        .route(
            "/api/beads/:id/transition",
            post(routes::transition_bead::<R, SQ>),
        )
        .route("/api/issues", get(routes::list_issues::<R, SQ>))
        // hq-fe-api-r.7 — derived snapshot of mayor attach state. Read-only over the
        // active-session registry; heartbeat freshness deferred (see dto).
        .route("/api/mayor/status", get(routes::mayor_status::<R, SQ>))
        .route("/api/worktrees", get(routes::list_worktrees::<R, SQ>))
        .route(
            "/api/worktrees/stream",
            get(routes::worktrees_stream::<R, SQ>),
        )
        .route("/api/nudge", post(routes::nudge::<R, SQ>))
        // hq-fe-api-w.9 — convoy write surface. POST creates + launches; the per-member
        // fail route halts a stuck convoy with an operator-supplied reason. `pause` /
        // `resume` are deferred — domain has no Pause/Resume commands today.
        .route("/api/convoys", post(routes::create_convoy::<R, SQ>))
        .route(
            "/api/convoys/:convoy/members/:member/fail",
            post(routes::fail_convoy_member::<R, SQ>),
        )
        // hq-fe-api-w.10 — promote quota.rotate / quota.retire from MCP-only to HTTP.
        .route(
            "/api/quota/accounts/:id/rotate",
            post(routes::quota_rotate::<R, SQ>),
        )
        .route(
            "/api/quota/accounts/:id/retire",
            post(routes::quota_retire::<R, SQ>),
        )
        .route("/api/stream", get(routes::stream::<R, SQ>))
        .with_state(state)
        // Idempotency-Key middleware (hq-fe-api-w.2). Layered between routes and auth so
        // an unauthorised retry never poisons the cache (auth runs first); replays still
        // re-enter the auth middleware so revoked tokens fail every retry.
        .layer(axum::middleware::from_fn_with_state(
            idempotency,
            idempotency_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            layer,
            auth::auth_middleware,
        ));

    let probes = Router::new()
        .route("/health", get(health::health))
        .route("/readyz", get(health::readyz))
        .route("/metrics", get(routes::metrics))
        .with_state(readiness);

    Router::new().merge(api).merge(probes)
}
