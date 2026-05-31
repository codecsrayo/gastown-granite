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
pub mod jwt;
pub mod comments;
pub mod control;
pub mod login;
pub mod rate_limit;
pub mod routes;
pub mod scope;
pub mod state;
pub mod stream;
pub mod term;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::routing::{delete, get, patch, post};
use axum::Router;
use tower_http::services::{ServeDir, ServeFile};

use gt_agent::SessionQueries;
use gt_beads::BeadRepository;
use gt_merge::MergeRepository;

pub use audit::{InMemoryWebAudit, JsonlWebAudit, WebAuditEvent, WebAuditSink};
pub use auth::{Actor, AuthClaims, AuthConfig, AuthLayer};
pub use jwt::{Claims, JwtError, JwtIssuer, DEFAULT_TTL, ISSUER};
// hq-fe-rbac.2 — re-export the unified RBAC config so callers (tests, future login
// route) don't have to depend on gt-rbac directly.
pub use gt_rbac::{ActorSpec, RbacConfig, RoleSpec, WebGrant};
pub use health::{HydrationHandle, ReadinessGate, ReadinessGateBuilder};
pub use idempotency::{idempotency_middleware, IdempotencyStore};
// hq-fe-auth.3 — typed payloads for `quota.login_*` SSE kinds. Re-exported so the
// integration tests and downstream crates can decode wire frames without touching
// `dto::` directly.
pub use dto::{
    QuotaLoginComplete, QuotaLoginEvent, QuotaLoginFailed, QuotaLoginStarted, QuotaLoginUrlReady,
};
pub use login::{LoginConfig, LoginRegistry, LoginStartResponse, LoginTokenRequest};
pub use rate_limit::{rate_limit_middleware, RateLimitStore};
pub use scope::{scope_middleware, RouteContext, ScopeGuard};
pub use comments::{DoltIssueCommenter, InMemoryIssueCommenter, IssueCommenter};
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
pub fn router<R, SQ, M>(
    state: AppState<R, SQ, M>,
    auth: AuthConfig,
    audit: Arc<dyn WebAuditSink>,
    readiness: ReadinessGate,
) -> Router
where
    R: BeadRepository + Send + Sync + 'static,
    SQ: SessionQueries + Send + Sync + 'static,
    M: MergeRepository + Send + Sync + 'static,
{
    router_with_stores(
        state,
        auth,
        audit,
        readiness,
        IdempotencyStore::with_defaults(),
        RateLimitStore::with_defaults(),
    )
}

/// `router` variant that lets the caller inject a pre-built idempotency store. Defaults
/// the rate-limit store; tests that need to assert the 429 path use
/// [`router_with_stores`] instead.
pub fn router_with_idempotency<R, SQ, M>(
    state: AppState<R, SQ, M>,
    auth: AuthConfig,
    audit: Arc<dyn WebAuditSink>,
    readiness: ReadinessGate,
    idempotency: IdempotencyStore,
) -> Router
where
    R: BeadRepository + Send + Sync + 'static,
    SQ: SessionQueries + Send + Sync + 'static,
    M: MergeRepository + Send + Sync + 'static,
{
    router_with_stores(
        state,
        auth,
        audit,
        readiness,
        idempotency,
        RateLimitStore::with_defaults(),
    )
}

/// Full `router` constructor with both stores injected — production boot uses [`router`]
/// (defaults both); tests reuse this so they can pin a tighter budget on either layer
/// and assert directly on the cache / counter state.
pub fn router_with_stores<R, SQ, M>(
    state: AppState<R, SQ, M>,
    auth: AuthConfig,
    audit: Arc<dyn WebAuditSink>,
    readiness: ReadinessGate,
    idempotency: IdempotencyStore,
    rate_limit: RateLimitStore,
) -> Router
where
    R: BeadRepository + Send + Sync + 'static,
    SQ: SessionQueries + Send + Sync + 'static,
    M: MergeRepository + Send + Sync + 'static,
{
    let layer = AuthLayer { config: auth, audit };
    // hq-fe-rbac.3 — per-route scope guards. Each closure builds a fresh `from_fn_with_state`
    // layer carrying the audit sink + a static scope string; only JWT-mode requests carry
    // `AuthClaims` so Bearer/Open posture grandfathers through (see `scope` module doc).
    //
    // hq-fe-skills.4 — the guard also captures the live skills handle. When the static
    // claim scopes do not carry `scope`, the middleware falls back to a dynamic union
    // over the claim's roles via the skills actor; an enabled skill widens the scope set
    // without a token re-issue. The clone is cheap (mpsc::Sender) and `None` keeps the
    // pre-`.4` posture (static-only).
    let skills_for_guard = state.skills.clone();
    let req = |scope: &'static str| {
        axum::middleware::from_fn_with_state(
            ScopeGuard {
                audit: layer.audit.clone(),
                scope,
                skills: skills_for_guard.clone(),
            },
            scope_middleware,
        )
    };
    let api = Router::new()
        .route(
            "/api/sessions",
            get(routes::list_sessions::<R, SQ, M>).route_layer(req("sessions.read")),
        )
        // hq-fe-api-w.6 — operator e-stop on a runaway polecat. Tmux-side kill goes
        // through `AppState.control` (a thin port over `gt_polecat::Tmux`); the registry
        // close happens via the existing `AgentEvent::Killed` projector path so SSE
        // subscribers see the same `agent.killed` record the in-process reactor produces.
        .route(
            "/api/sessions/:id",
            delete(routes::delete_session::<R, SQ, M>).route_layer(req("sessions.write")),
        )
        // hq-fe-api-w.8 — softer e-stop: `tmux send-keys Escape` cancels the agent's
        // in-flight turn without killing the polecat. The same `PolecatControl` port
        // handles both ops so the production wiring stays a single shared `TmuxCli`.
        .route(
            "/api/sessions/:id/interrupt",
            post(routes::interrupt_session::<R, SQ, M>).route_layer(req("sessions.write")),
        )
        // hq-fe-term.2 — dashboard dock-terminal WebSocket. Upgrades to a binary
        // duplex stream over a live tmux session; bytes both directions. Behind its
        // own scope (`terminal.attach`) so a quota.write grant does not implicitly
        // hand out shell access.
        .route(
            "/api/sessions/:id/term",
            get(term::term_attach::<R, SQ, M>).route_layer(req("terminal.attach")),
        )
        // hq-fe-api-w.7 — cold-restart: respawn a stuck polecat with the same hook
        // bead + convoy by reading env from the dying session before tearing it down.
        // Emits the close+reopen `AgentEvent` pair the supervisor would on a real
        // restart so SSE subscribers + projector see a single atomic transition.
        .route(
            "/api/sessions/:id/restart",
            post(routes::restart_session::<R, SQ, M>).route_layer(req("sessions.write")),
        )
        // hq-fe-api-w.3: write surface on the dispatcher's bead table — POST mints a
        // `pending` row (wrapping scheduling.create_bead), PATCH partially updates
        // title/priority/assignee. Status transitions stay on the reactor.
        .route(
            "/api/beads",
            get(routes::list_beads::<R, SQ, M>).route_layer(req("beads.read")),
        )
        .route(
            "/api/beads/:id",
            patch(routes::update_bead::<R, SQ, M>).route_layer(req("beads.write")),
        )
        // hq-fe-api-w.4 — operator override for the bead state machine. Not a reactor:
        // dispatcher capacity stays unchanged so a real worker's lifecycle is not double-
        // counted. See [`routes::transition_bead`] for the allowed transition matrix.
        .route(
            "/api/beads/:id/transition",
            post(routes::transition_bead::<R, SQ, M>).route_layer(req("beads.write")),
        )
        // hq-fe-api-w.5 — append-only operator comments. Writes to the
        // `hq.issues.notes` column via `AppState.commenter`; the route formats
        // a canonical fragment so the column stays parseable for a future
        // migration to a structured `issue_comments` table.
        .route(
            "/api/beads/:id/comments",
            post(routes::comment_bead::<R, SQ, M>).route_layer(req("beads.write")),
        )
        .route(
            "/api/issues",
            get(routes::list_issues::<R, SQ, M>).route_layer(req("beads.read")),
        )
        // hq-fe-skills.2 — registered skills catalog + per-role bindings. Read-only
        // mirrors of the `gt_skills` actor's `skills()` / `bindings()` snapshots.
        // Both routes share `skills.read` so a single grant covers the dashboard's
        // RoleList/SkillToggle hydration; `.3` will introduce a sibling `skills.write`
        // for the toggle POST surface.
        .route(
            "/api/skills",
            get(routes::list_skills::<R, SQ, M>).route_layer(req("skills.read")),
        )
        .route(
            "/api/roles",
            get(routes::list_roles::<R, SQ, M>).route_layer(req("skills.read")),
        )
        // hq-fe-skills.3 — toggle a single skill on/off for a role. Sibling
        // `skills.write` scope so a `skills.read` token can hydrate but never mutate.
        // Idempotent: re-asserting the existing state returns 200 without dispatching
        // an event, so the dashboard's optimistic toggle is safe to replay.
        .route(
            "/api/roles/:role/skills",
            post(routes::toggle_role_skill::<R, SQ, M>).route_layer(req("skills.write")),
        )
        // hq-fe-api-r.7 — derived snapshot of mayor attach state. Read-only over the
        // active-session registry; heartbeat freshness deferred (see dto).
        .route(
            "/api/mayor/status",
            get(routes::mayor_status::<R, SQ, M>).route_layer(req("sessions.read")),
        )
        // hq-fe-api-r.4 — snapshot of the merge slot board (ready/merging/merged/failed).
        // Read-only; backed by the same `MergeRepository` the actor upserts on each
        // transition. Deltas flow on the existing SSE `merge.*` channel.
        .route(
            "/api/merges",
            get(routes::list_merges::<R, SQ, M>).route_layer(req("merge.read")),
        )
        // hq-fe-rbac.4 — identity bootstrap for the dashboard. No scope guard: every
        // authenticated actor needs to learn its own `actor/mode/roles/scopes`, and the
        // route never returns anything beyond the request's own claims.
        .route("/api/whoami", get(routes::whoami::<R, SQ, M>))
        .route(
            "/api/worktrees",
            get(routes::list_worktrees::<R, SQ, M>).route_layer(req("worktrees.read")),
        )
        .route(
            "/api/worktrees/stream",
            get(routes::worktrees_stream::<R, SQ, M>).route_layer(req("worktrees.read")),
        )
        .route(
            "/api/nudge",
            post(routes::nudge::<R, SQ, M>).route_layer(req("nudge.write")),
        )
        // hq-fe-api-w.9 — convoy write surface. POST creates + launches; the per-member
        // fail route halts a stuck convoy with an operator-supplied reason. `pause` /
        // `resume` are deferred — domain has no Pause/Resume commands today.
        .route(
            "/api/convoys",
            get(routes::list_convoys::<R, SQ, M>).route_layer(req("convoys.read")),
        )
        .route(
            "/api/convoys/:convoy/members/:member/fail",
            post(routes::fail_convoy_member::<R, SQ, M>).route_layer(req("convoys.write")),
        )
        // hq-fe-api-r.1 — flat snapshot of every account in the quota registry. Powers
        // the dashboard sidebar (hq-fe-view.10) AccountCard + QuotaMeter + RotationChips.
        .route(
            "/api/quota/accounts",
            get(routes::quota_accounts::<R, SQ, M>).route_layer(req("quota.read")),
        )
        // hq-fe-api-w.10 — promote quota.rotate / quota.retire from MCP-only to HTTP.
        .route(
            "/api/quota/accounts/:id/rotate",
            post(routes::quota_rotate::<R, SQ, M>).route_layer(req("quota.write")),
        )
        .route(
            "/api/quota/accounts/:id/retire",
            post(routes::quota_retire::<R, SQ, M>).route_layer(req("quota.write")),
        )
        // hq-fe-api-r.2 — composite snapshot for the rotation panel: live Cooldown
        // accounts (`waiting_unlock`) joined with the tail of `quota.rotated` records
        // pulled from the shared `events.jsonl` (`recent_rotations`).
        .route(
            "/api/quota/rotation",
            get(routes::quota_rotation::<R, SQ, M>).route_layer(req("quota.read")),
        )
        // hq-fe-auth.2 — account login flow (PTY-driven `claude /login`). Three POSTs on
        // the same prefix so a single scope grant (`quota.write`) covers the full flow.
        // Events surface as `quota.login_*` `EventRecord`s on the existing events
        // broadcast; `.3` defines their SSE rendering.
        .route(
            "/api/quota/accounts/:id/login",
            post(login::login_start::<R, SQ, M>).route_layer(req("quota.write")),
        )
        .route(
            "/api/quota/accounts/:id/login/token",
            post(login::login_token::<R, SQ, M>).route_layer(req("quota.write")),
        )
        .route(
            "/api/quota/accounts/:id/login/cancel",
            post(login::login_cancel::<R, SQ, M>).route_layer(req("quota.write")),
        )
        .route(
            "/api/stream",
            get(routes::stream::<R, SQ, M>).route_layer(req("feed.read")),
        )
        // hq-fe-api-r.5 — historical replay of the same events.jsonl `/api/stream` ships;
        // dashboard seeds its activity store from this before subscribing to SSE.
        .route(
            "/api/feed",
            get(routes::feed::<R, SQ, M>).route_layer(req("feed.read")),
        )
        .with_state(state.clone());

    // hq-fe-rbac.3 — POST handlers on dual-method paths (`/api/beads`, `/api/convoys`)
    // live on a sibling router so each method gets its own scope guard. Stacking
    // `route_layer` calls on a `MethodRouter` wraps *every* method in scope (axum's
    // MR layer is per-router, not per-method), which would force a reader token to also
    // carry the write scope on the GET path. The merge route below keeps `beads.read`
    // limited to the GET handler and `beads.write` limited to the POST handler.
    let writes = Router::new()
        .route(
            "/api/beads",
            post(routes::create_bead::<R, SQ, M>).route_layer(req("beads.write")),
        )
        .route(
            "/api/convoys",
            post(routes::create_convoy::<R, SQ, M>).route_layer(req("convoys.write")),
        )
        .with_state(state.clone());

    // hq-fe-api-w.11 — rate-limited bulk-create surface. The per-actor counter sits on
    // a sibling sub-router so the layer applies only to the bulk path; the rest of the
    // /api surface is unchanged. Merged into `api` *before* idempotency + auth so the
    // global layers still wrap every request.
    let bulk = Router::new()
        .route(
            "/api/beads/bulk",
            post(routes::create_beads_bulk::<R, SQ, M>).route_layer(req("beads.write")),
        )
        .with_state(state)
        .layer(axum::middleware::from_fn_with_state(
            rate_limit,
            rate_limit_middleware,
        ));

    let api = api
        .merge(writes)
        .merge(bulk)
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

/// hq-fe-cut.1 — attach the SvelteKit static build as the router's fallback service.
///
/// The dashboard SPA (`apps/web/build`) is served from `/` and `/_app/*`. Any request that
/// does not match a declared route (`/api/*`, `/health`, `/readyz`, `/metrics`) lands in
/// the fallback: [`ServeDir`] resolves the file off disk; missing files (including every
/// SvelteKit history-mode URL like `/sessions/abc`) fall through to `index.html` so the
/// client router can take over. Static assets sit **outside** [`auth::auth_middleware`]
/// on purpose — the login page itself is part of the SPA and cannot require a bearer to
/// load.
///
/// Returns the input router untouched when `dist` does not exist on disk; production boot
/// logs and continues so a missing build artefact does not block the API. Tests opt in
/// by passing a populated temp dir.
pub fn with_static_assets(router: Router, dist: impl AsRef<Path>) -> Router {
    let dist = dist.as_ref();
    if !dist.exists() {
        eprintln!(
            "[gt-web] static dist missing ({}) — only /api + probes will respond",
            dist.display()
        );
        return router;
    }
    let index = dist.join("index.html");
    let serve = ServeDir::new(dist).fallback(ServeFile::new(index));
    router.fallback_service(serve)
}

/// Resolve the SvelteKit build directory from `GT_WEB_DIST`, defaulting to the in-repo
/// path when the env var is unset. Returns `None` only if the var is set to an empty
/// string (explicit opt-out — useful for the `/api`-only test rigs).
pub fn dist_from_env() -> Option<PathBuf> {
    match std::env::var("GT_WEB_DIST") {
        Ok(s) if s.is_empty() => None,
        Ok(s) => Some(PathBuf::from(s)),
        Err(_) => Some(PathBuf::from("apps/web/build")),
    }
}
