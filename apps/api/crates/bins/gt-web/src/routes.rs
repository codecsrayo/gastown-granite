//! Read-side HTTP surface. Handlers translate domain types to DTOs and never expose internal
//! shapes. Generic over the bead/session ports so tests inject in-memory adapters and the
//! production bin plugs in Dolt/Postgres — same isolation rule as `bins/gt`.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};

use gt_agent::{AgentEvent, SessionQueries};
use gt_beads::{BeadRepository, BeadStatus};
use gt_events::Envelope;

use crate::dto::{BeadDto, BeadsQuery, NudgeRequest, NudgeResponse, SessionDto};
use crate::state::AppState;
use crate::stream::sse_from_receiver;

/// `GET /api/sessions` — snapshot of active sessions. The reader port lives in `gt-agent`;
/// the dashboard fetches this once and then patches rows via the SSE stream.
pub async fn list_sessions<R, SQ>(
    State(state): State<AppState<R, SQ>>,
) -> Result<Json<Vec<SessionDto>>, AppError>
where
    R: BeadRepository + Send + Sync + 'static,
    SQ: SessionQueries + Send + Sync + 'static,
{
    let rows = state.sessions.active_sessions().await.map_err(AppError::from)?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

/// `GET /api/beads?status=pending` — snapshot of beads in one status.
pub async fn list_beads<R, SQ>(
    State(state): State<AppState<R, SQ>>,
    Query(q): Query<BeadsQuery>,
) -> Result<Json<Vec<BeadDto>>, AppError>
where
    R: BeadRepository + Send + Sync + 'static,
    SQ: SessionQueries + Send + Sync + 'static,
{
    let status: BeadStatus = q
        .parsed()
        .ok_or_else(|| AppError::bad_request(format!("unknown status: {}", q.status)))?;
    let rows = state.beads.list_by_status(status).await.map_err(AppError::from)?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

/// `POST /api/nudge` — write-side: emits an `AgentEvent::Heartbeat` to the agent relay. The
/// reactor records it in the audit log; SSE subscribers see it as `agent.heartbeat`.
pub async fn nudge<R, SQ>(
    State(state): State<AppState<R, SQ>>,
    Json(req): Json<NudgeRequest>,
) -> Result<Json<NudgeResponse>, AppError>
where
    R: BeadRepository + Send + Sync + 'static,
    SQ: SessionQueries + Send + Sync + 'static,
{
    let env = Envelope::root(AgentEvent::Heartbeat { session: req.session });
    state
        .agent_events
        .send(env)
        .await
        .map_err(|_| AppError::internal("agent relay closed"))?;
    Ok(Json(NudgeResponse { accepted: true }))
}

/// `GET /api/stream` — SSE feed of the running root's broadcast.
pub async fn stream<R, SQ>(State(state): State<AppState<R, SQ>>) -> impl IntoResponse
where
    R: BeadRepository + Send + Sync + 'static,
    SQ: SessionQueries + Send + Sync + 'static,
{
    sse_from_receiver(state.events.subscribe())
}

/// `GET /metrics` — Prometheus text exposition. Stateless: scrapes the process-global
/// registry exposed by `gt-telemetry`, independent of the per-request `AppState`.
pub async fn metrics() -> Result<String, AppError> {
    gt_telemetry::metrics::render_text()
        .map_err(|e| AppError::internal(format!("metrics render: {e}")))
}

/// Single error type mapped to JSON + a status code. Domain errors collapse to 500 by design:
/// `gt-web` is a gateway, not the place to invent new error semantics.
#[derive(Debug)]
pub struct AppError {
    pub status: StatusCode,
    pub message: String,
}

impl AppError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: msg.into(),
        }
    }
    pub fn internal(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: msg.into(),
        }
    }
}

impl From<gt_events::AppError> for AppError {
    fn from(e: gt_events::AppError) -> Self {
        Self::internal(format!("{e}"))
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}

// Suppress unused-import lints on aliases brought in only for trait bounds.
#[allow(dead_code)]
fn _bounds_anchor<R: BeadRepository, SQ: SessionQueries>(_r: Arc<R>, _s: Arc<SQ>) {}
