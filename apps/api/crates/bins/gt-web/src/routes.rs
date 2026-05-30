//! Read-side HTTP surface. Handlers translate domain types to DTOs and never expose internal
//! shapes. Generic over the bead/session ports so tests inject in-memory adapters and the
//! production bin plugs in Dolt/Postgres — same isolation rule as `bins/gt`.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};

use gt_agent::{AgentEvent, SessionQueries};
use gt_beads::{Bead, BeadRepository, BeadStatus};
use gt_events::Envelope;
use gt_orchestration::{FailMember, LaunchConvoy, OrchCommand};
use gt_quota::{QuotaCommand, RotateAccount};
use gt_root::RootCommand;
use gt_store_dolt::{IssueFilter, IssueRow};

use crate::dto::{
    BeadCreateRequest, BeadDto, BeadTransitionRequest, BeadUpdateRequest, BeadsQuery,
    ConvoyCreateRequest, ConvoyCreateResponse, DirtyFileDto, IssueDto, IssuesQuery,
    MemberFailRequest, NudgeRequest, NudgeResponse, QuotaRetireResponse, QuotaRotateRequest,
    SessionDto, SessionsQuery, WorktreeDto,
};
use crate::state::AppState;
use crate::stream::{sse_from_json_receiver, sse_from_receiver};

/// `GET /api/sessions[?role=polecat][&rig=hq]` — snapshot of active sessions, optionally filtered by
/// role (hq-8iur.7). The reader port lives in `gt-agent`; the dashboard fetches this once and
/// then patches rows via the SSE stream. An unknown `role` value yields an empty result (it
/// matches no session) rather than an error — the filter is a view, not a command.
pub async fn list_sessions<R, SQ>(
    State(state): State<AppState<R, SQ>>,
    Query(q): Query<SessionsQuery>,
) -> Result<Json<Vec<SessionDto>>, AppError>
where
    R: BeadRepository + Send + Sync + 'static,
    SQ: SessionQueries + Send + Sync + 'static,
{
    let rows = state.sessions.active_sessions().await.map_err(AppError::from)?;
    let filtered: Vec<SessionDto> = rows
        .into_iter()
        .map(SessionDto::from)
        .filter(|d| q.role.as_deref().map_or(true, |r| d.role == r))
        .filter(|d| q.rig.as_deref().map_or(true, |r| d.rig == r))
        .collect();
    Ok(Json(filtered))
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

/// `DELETE /api/sessions/:id` — operator e-stop on a runaway polecat (hq-fe-api-w.6).
/// The route is the dashboard's "kill" button: it (a) confirms the session is still in
/// the active registry, (b) calls the [`crate::PolecatControl::kill`] port to terminate
/// the underlying tmux session, and (c) emits `AgentEvent::Killed` on the agent relay so
/// the projector flips the row to `Killed` and SSE subscribers see the lifecycle close.
///
/// Order matters: tmux kill goes *before* the event so a fatal edge error (missing
/// `tmux` binary, server unreachable) surfaces as 500 without leaving a half-closed
/// session row in the registry. A successful tmux kill followed by a relay drop is
/// recoverable — the session lingers as `Killed` in tmux but the registry will resync on
/// the next replay/restart (mirrors how `AgentEvent::SessionEnd` is handled today).
pub async fn delete_session<R, SQ>(
    State(state): State<AppState<R, SQ>>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError>
where
    R: BeadRepository + Send + Sync + 'static,
    SQ: SessionQueries + Send + Sync + 'static,
{
    if id.is_empty() {
        return Err(AppError::bad_request("session id is empty"));
    }
    let active = state.sessions.active_sessions().await.map_err(AppError::from)?;
    if !active.iter().any(|s| s.id == id) {
        return Err(AppError::not_found(format!("session {id}")));
    }
    let control = state
        .control
        .as_ref()
        .ok_or_else(|| AppError::internal("polecat control not wired"))?;
    control.kill(&id)?;
    let env = Envelope::root(AgentEvent::Killed {
        session: id.clone(),
        reason: "operator: DELETE /api/sessions/:id".to_string(),
    });
    state
        .agent_events
        .send(env)
        .await
        .map_err(|_| AppError::internal("agent relay closed"))?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/sessions/:id/interrupt` — softer e-stop (hq-fe-api-w.8). Sends `Escape`
/// through tmux `send-keys`, which cancels the coding agent's in-flight turn without
/// ending the polecat. Same registry pre-check as `delete_session` (404 on unknown id),
/// no `AgentEvent` emit: the lifecycle row stays in its current state because the
/// polecat is still alive — only the agent's current message is cancelled. Returns 204
/// on success; idempotent at the route layer since the registry check absorbs repeats
/// against an already-dead session.
pub async fn interrupt_session<R, SQ>(
    State(state): State<AppState<R, SQ>>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError>
where
    R: BeadRepository + Send + Sync + 'static,
    SQ: SessionQueries + Send + Sync + 'static,
{
    if id.is_empty() {
        return Err(AppError::bad_request("session id is empty"));
    }
    let active = state.sessions.active_sessions().await.map_err(AppError::from)?;
    if !active.iter().any(|s| s.id == id) {
        return Err(AppError::not_found(format!("session {id}")));
    }
    let control = state
        .control
        .as_ref()
        .ok_or_else(|| AppError::internal("polecat control not wired"))?;
    control.send_keys(&id, &["Escape"])?;
    Ok(StatusCode::NO_CONTENT)
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

/// `POST /api/beads` — mint a `pending` bead in the dispatcher queue (hq-fe-api-w.3).
/// Thin HTTP wrapper around `scheduling.create_bead`: same edge op the MCP tool drives,
/// so audit and idempotency-key replay flow uniformly. Returns the persisted row so the
/// dashboard can append it to the kanban without a follow-up `GET /api/beads`.
pub async fn create_bead<R, SQ>(
    State(state): State<AppState<R, SQ>>,
    Json(req): Json<BeadCreateRequest>,
) -> Result<(StatusCode, Json<BeadDto>), AppError>
where
    R: BeadRepository + Send + Sync + 'static,
    SQ: SessionQueries + Send + Sync + 'static,
{
    if req.id.is_empty() {
        return Err(AppError::bad_request("bead id is empty"));
    }
    if req.title.is_empty() {
        return Err(AppError::bad_request("bead title is empty"));
    }
    if req.priority > 2 {
        return Err(AppError::bad_request(format!(
            "priority must be 0..=2, got {}",
            req.priority
        )));
    }
    let bus = state
        .bus
        .as_ref()
        .ok_or_else(|| AppError::internal("command bus not wired"))?;
    let mut bead = Bead::new(&req.id, &req.title, BeadStatus::Pending, req.priority);
    bead.assignee = req
        .assignee
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    bus.sched()
        .create_bead(bead.clone())
        .await
        .map_err(AppError::from)?;
    Ok((StatusCode::CREATED, Json(BeadDto::from(bead))))
}

/// `PATCH /api/beads/:id` — partial update of `title`, `priority`, or `assignee`
/// (hq-fe-api-w.3). Status changes go through the dispatcher reactor — the route refuses
/// `status` in the body so the kanban can't bypass the lifecycle. Reads-then-upserts
/// because `BeadRepository` does not ship a `Patch` method; the write races a concurrent
/// CAS claim, in which case the next claim's reducer wins (the field changes are visible
/// on the next snapshot read).
pub async fn update_bead<R, SQ>(
    State(state): State<AppState<R, SQ>>,
    Path(id): Path<String>,
    Json(req): Json<BeadUpdateRequest>,
) -> Result<Json<BeadDto>, AppError>
where
    R: BeadRepository + Send + Sync + 'static,
    SQ: SessionQueries + Send + Sync + 'static,
{
    if id.is_empty() {
        return Err(AppError::bad_request("bead id is empty"));
    }
    if req.is_empty() {
        return Err(AppError::bad_request(
            "patch is empty (nothing to update)",
        ));
    }
    if let Some(p) = req.priority {
        if p > 2 {
            return Err(AppError::bad_request(format!(
                "priority must be 0..=2, got {p}"
            )));
        }
    }
    if matches!(&req.title, Some(s) if s.is_empty()) {
        return Err(AppError::bad_request("title is empty"));
    }
    let existing = state
        .beads
        .get(&id)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found(format!("bead {id}")))?;
    let mut updated = existing;
    if let Some(t) = req.title {
        updated.title = t;
    }
    if let Some(p) = req.priority {
        updated.priority = p;
    }
    if let Some(a) = req.assignee {
        updated.assignee = if a.is_empty() { None } else { Some(a) };
    }
    state.beads.upsert(&updated).await.map_err(AppError::from)?;
    Ok(Json(BeadDto::from(updated)))
}

/// `POST /api/beads/:id/transition` — operator override for the bead state machine
/// (hq-fe-api-w.4). Flips `BeadStatus` in-place via `BeadRepository::upsert`, gated by
/// [`is_operator_transition_allowed`]. The route is **not** a reactor: it does not
/// consume/free scheduler capacity, register patrols, or emit `MergeEvent`s — those
/// stay on the reactor path (`scheduling.mark_dispatched`, `MergeEvent::Merged`, etc.).
/// Operator-driven moves are intentionally restricted to:
///
/// - `pending → working|done|failed` (kanban "claim/close")
/// - `dispatched → pending|failed`   (release a stuck claim manually)
/// - `working → pending|done|failed` (release/close in-flight work)
/// - `done → pending` and `failed → pending` (re-open + retry)
///
/// Forbidden: self-transitions, `pending → dispatched` (scheduler-owned), and crossing
/// `done` ↔ `failed` directly (must round-trip through `pending` so the re-open is
/// explicit in the audit log).
pub async fn transition_bead<R, SQ>(
    State(state): State<AppState<R, SQ>>,
    Path(id): Path<String>,
    Json(req): Json<BeadTransitionRequest>,
) -> Result<Json<BeadDto>, AppError>
where
    R: BeadRepository + Send + Sync + 'static,
    SQ: SessionQueries + Send + Sync + 'static,
{
    if id.is_empty() {
        return Err(AppError::bad_request("bead id is empty"));
    }
    let target = BeadStatus::parse(&req.to)
        .ok_or_else(|| AppError::bad_request(format!("unknown status: {}", req.to)))?;
    let existing = state
        .beads
        .get(&id)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found(format!("bead {id}")))?;
    if !is_operator_transition_allowed(existing.status, target) {
        return Err(AppError::bad_request(format!(
            "transition {} -> {} not permitted for operator override",
            existing.status.as_str(),
            target.as_str()
        )));
    }
    let mut updated = existing;
    updated.status = target;
    state.beads.upsert(&updated).await.map_err(AppError::from)?;
    Ok(Json(BeadDto::from(updated)))
}

/// State-machine guard for [`transition_bead`]. Returns `true` only for transitions an
/// operator is allowed to drive manually through the HTTP route. Scheduler-owned moves
/// and self-transitions are rejected here so the caller surfaces a 400 with the source
/// and target verbatim (same shape as `gt-mcp`'s `issues.transition`).
fn is_operator_transition_allowed(from: BeadStatus, to: BeadStatus) -> bool {
    use BeadStatus::*;
    if from == to {
        return false;
    }
    match (from, to) {
        (Pending, Working) | (Pending, Done) | (Pending, Failed) => true,
        (Dispatched, Pending) | (Dispatched, Failed) => true,
        (Working, Pending) | (Working, Done) | (Working, Failed) => true,
        (Done, Pending) | (Failed, Pending) => true,
        _ => false,
    }
}

/// `POST /api/convoys` — create + launch a convoy (hq-fe-api-w.9). Thin HTTP wrapper
/// around `OrchCommand::Launch`: same edge op `orch.launch_convoy` drives, so audit and
/// idempotency-key replay flow uniformly. A successful response means the convoy is
/// live and its first member already dispatched (the orchestrator does both atomically).
///
/// `pause` / `resume` are deliberately absent: the orchestration domain has no Pause /
/// Resume commands today (`gap parcial` in the migration plan). `members/:m/fail` ships
/// on [`fail_convoy_member`] in the same epic.
pub async fn create_convoy<R, SQ>(
    State(state): State<AppState<R, SQ>>,
    Json(req): Json<ConvoyCreateRequest>,
) -> Result<(StatusCode, Json<ConvoyCreateResponse>), AppError>
where
    R: BeadRepository + Send + Sync + 'static,
    SQ: SessionQueries + Send + Sync + 'static,
{
    if req.convoy.is_empty() {
        return Err(AppError::bad_request("convoy id is empty"));
    }
    if req.members.is_empty() {
        return Err(AppError::bad_request("convoy has no members"));
    }
    if req.members.iter().any(|m| m.is_empty()) {
        return Err(AppError::bad_request("convoy member id is empty"));
    }
    let bus = state
        .bus
        .as_ref()
        .ok_or_else(|| AppError::internal("command bus not wired"))?;
    let cmd = RootCommand::Orch(OrchCommand::Launch(LaunchConvoy {
        convoy: req.convoy.clone(),
        members: req.members.clone(),
    }));
    bus.dispatch(cmd, None).await.map_err(AppError::from)?;
    Ok((
        StatusCode::CREATED,
        Json(ConvoyCreateResponse {
            convoy: req.convoy,
            members: req.members,
            launched: true,
        }),
    ))
}

/// `POST /api/convoys/:convoy/members/:member/fail` — halt a convoy at the failing
/// member (hq-fe-api-w.9). Wraps `OrchCommand::Fail` so the same `MemberFailed` /
/// `ConvoyFailed` event chain the MCP tool produces flows through the audit log and
/// SSE. The body carries the human-readable reason (operator's "why"); path params
/// disambiguate the target so the URL itself is a self-describing audit entry.
pub async fn fail_convoy_member<R, SQ>(
    State(state): State<AppState<R, SQ>>,
    Path((convoy, member)): Path<(String, String)>,
    Json(req): Json<MemberFailRequest>,
) -> Result<Json<serde_json::Value>, AppError>
where
    R: BeadRepository + Send + Sync + 'static,
    SQ: SessionQueries + Send + Sync + 'static,
{
    if convoy.is_empty() {
        return Err(AppError::bad_request("convoy id is empty"));
    }
    if member.is_empty() {
        return Err(AppError::bad_request("member id is empty"));
    }
    if req.reason.trim().is_empty() {
        return Err(AppError::bad_request("reason is empty"));
    }
    let bus = state
        .bus
        .as_ref()
        .ok_or_else(|| AppError::internal("command bus not wired"))?;
    let cmd = RootCommand::Orch(OrchCommand::Fail(FailMember {
        convoy: convoy.clone(),
        member: member.clone(),
        reason: req.reason.clone(),
    }));
    bus.dispatch(cmd, None).await.map_err(AppError::from)?;
    Ok(Json(serde_json::json!({
        "failed": true,
        "convoy": convoy,
        "member": member,
        "reason": req.reason,
    })))
}

/// `POST /api/quota/accounts/:id/rotate` — promote `quota.rotate` from MCP-only to an
/// HTTP route (hq-fe-api-w.10). Dispatches through the same [`gt_root::CommandBus`] the
/// gt-mcp tools drive, so audit and scope flow uniformly. Path `:id` is the source
/// account (the one being rotated *away from*); the body carries the healthy target.
pub async fn quota_rotate<R, SQ>(
    State(state): State<AppState<R, SQ>>,
    Path(id): Path<String>,
    Json(req): Json<QuotaRotateRequest>,
) -> Result<Json<serde_json::Value>, AppError>
where
    R: BeadRepository + Send + Sync + 'static,
    SQ: SessionQueries + Send + Sync + 'static,
{
    if id.is_empty() {
        return Err(AppError::bad_request("account id is empty"));
    }
    if req.to_account.trim().is_empty() {
        return Err(AppError::bad_request("to_account is empty"));
    }
    if req.to_account == id {
        return Err(AppError::bad_request(
            "to_account must differ from the source",
        ));
    }
    let bus = state
        .bus
        .as_ref()
        .ok_or_else(|| AppError::internal("command bus not wired"))?;
    let cmd = RootCommand::Quota(QuotaCommand::Rotate(RotateAccount {
        from_account: id.clone(),
        to_account: req.to_account.clone(),
        now_secs: req.now_secs.unwrap_or_else(epoch_now),
    }));
    bus.dispatch(cmd, None).await.map_err(AppError::from)?;
    Ok(Json(serde_json::json!({
        "rotated": true,
        "from": id,
        "to": req.to_account,
    })))
}

/// `POST /api/quota/accounts/:id/retire` — drop an account from the live registry
/// (hq-fe-api-w.10). Mirrors `quota.retire` (the edge op that bypasses `QuotaCommand`
/// because account-registration is not event-logged in the domain — see
/// [`gt_mcp::RetireAccount`]). Idempotent: a missing account returns `removed: false`
/// with HTTP 200 so callers can drive retire-loops without conditional logic.
pub async fn quota_retire<R, SQ>(
    State(state): State<AppState<R, SQ>>,
    Path(id): Path<String>,
) -> Result<Json<QuotaRetireResponse>, AppError>
where
    R: BeadRepository + Send + Sync + 'static,
    SQ: SessionQueries + Send + Sync + 'static,
{
    if id.is_empty() {
        return Err(AppError::bad_request("account id is empty"));
    }
    let bus = state
        .bus
        .as_ref()
        .ok_or_else(|| AppError::internal("command bus not wired"))?;
    let removed = bus.quota().remove_account(id.clone()).await;
    Ok(Json(QuotaRetireResponse { account: id, removed }))
}

fn epoch_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `GET /api/stream` — SSE feed of the running root's broadcast.
pub async fn stream<R, SQ>(State(state): State<AppState<R, SQ>>) -> impl IntoResponse
where
    R: BeadRepository + Send + Sync + 'static,
    SQ: SessionQueries + Send + Sync + 'static,
{
    sse_from_receiver(state.events.subscribe())
}

/// `GET /api/issues` — thin HTTP mirror of the `gt://issues` MCP resource (hq-fe-api-r.9).
/// Reads the canonical `hq.issues` table (25 cols, distinct from `BeadRepository`'s `beads`
/// table — see [`crate::dto::IssueDto`]). Returns `[]` when `AppState.issues` is unset so
/// the in-memory dev mode keeps working without a Dolt connection; same posture as
/// `/api/worktrees`. Query string filters mirror the MCP grammar to keep both surfaces
/// reading the same bead slice.
pub async fn list_issues<R, SQ>(
    State(state): State<AppState<R, SQ>>,
    Query(q): Query<IssuesQuery>,
) -> Result<Json<Vec<IssueDto>>, AppError>
where
    R: BeadRepository + Send + Sync + 'static,
    SQ: SessionQueries + Send + Sync + 'static,
{
    let Some(issues) = state.issues.clone() else {
        return Ok(Json(Vec::new()));
    };
    let mut filter = IssueFilter::default();
    if let Some(s) = q.status.as_deref() {
        filter.status = s
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect();
    }
    filter.assignee = q.assignee;
    filter.external_ref = q.external_ref;
    filter.issue_type = q.issue_type;
    filter.priority_max = q.priority_max;
    filter.limit = q.limit;
    let rows = issues
        .list(&filter)
        .await
        .map_err(|e| AppError::internal(format!("issues list: {e}")))?;
    Ok(Json(rows.into_iter().map(IssueDto::from).collect()))
}

impl From<IssueRow> for IssueDto {
    fn from(r: IssueRow) -> Self {
        Self {
            id: r.id,
            title: r.title,
            status: r.status,
            priority: r.priority,
            issue_type: r.issue_type,
            assignee: r.assignee,
            owner: r.owner,
            external_ref: r.external_ref,
            created_at: r.created_at,
            updated_at: r.updated_at,
            closed_at: r.closed_at,
        }
    }
}

/// `GET /api/worktrees` — snapshot of every git worktree under the town root, with branch,
/// HEAD, ahead/behind vs. `main`, and the dirty file list (hq-fe-api-r.8). Read-only: shells
/// `git` with `tokio::process` against `state.town_root`. Returns `[]` when no town root is
/// configured (the gateway does not invent one) and surfaces shell failures as 500 — same
/// fail-fast posture as the other handlers.
pub async fn list_worktrees<R, SQ>(
    State(state): State<AppState<R, SQ>>,
) -> Result<Json<Vec<WorktreeDto>>, AppError>
where
    R: BeadRepository + Send + Sync + 'static,
    SQ: SessionQueries + Send + Sync + 'static,
{
    let Some(root) = state.town_root.clone() else {
        return Ok(Json(Vec::new()));
    };
    let root: &std::path::Path = root.as_ref();
    let dtos = collect_worktrees(root).await?;
    Ok(Json(dtos))
}

/// `GET /api/worktrees/stream` — SSE feed of full snapshots (hq-fe-api-r.12). The bin
/// spawns one polling task per process that shells `git` every 2s and broadcasts the
/// snapshot; this handler subscribes per connection and serializes each broadcast frame as
/// one SSE event. When `worktrees_stream` is unset (no `GT_TOWN_ROOT`) the connection
/// short-circuits with 503 so clients fall back to the snapshot endpoint instead of
/// hanging on a never-firing channel.
pub async fn worktrees_stream<R, SQ>(
    State(state): State<AppState<R, SQ>>,
) -> Result<axum::response::Response, AppError>
where
    R: BeadRepository + Send + Sync + 'static,
    SQ: SessionQueries + Send + Sync + 'static,
{
    let Some(tx) = state.worktrees_stream.clone() else {
        return Err(AppError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: "worktrees stream not wired (set GT_TOWN_ROOT)".into(),
        });
    };
    Ok(sse_from_json_receiver(tx.subscribe()).into_response())
}

/// Snapshot the town root's worktrees + per-worktree git state. Shared between the
/// `GET /api/worktrees` handler (one-shot fetch) and the SSE poller in `main.rs`
/// (hq-fe-api-r.12). `pub(crate)` rather than `pub` because no consumer outside this bin
/// has a reason to shell git on its behalf.
pub async fn collect_worktrees(
    root: &std::path::Path,
) -> Result<Vec<WorktreeDto>, AppError> {
    let list_out = run_git(root, &["worktree", "list", "--porcelain"]).await?;
    let entries = parse_worktree_list(&list_out);

    let mut out = Vec::with_capacity(entries.len());
    let mut first = true;
    for entry in entries {
        // `git worktree list --porcelain` always emits the main worktree first; flag it so the
        // frontend can render it on top without re-parsing the path.
        let is_main = first;
        first = false;

        let wt_path = std::path::Path::new(&entry.path);
        let dirty = collect_dirty(wt_path).await?;
        let (ahead, behind) = ahead_behind(wt_path).await?;
        let (head_subject, head_author, head_time) = head_commit_meta(wt_path).await;

        out.push(WorktreeDto {
            path: entry.path,
            branch: entry.branch,
            head: entry.head,
            is_main,
            ahead,
            behind,
            dirty,
            head_subject,
            head_author,
            head_time,
        });
    }
    Ok(out)
}

/// HEAD commit subject + author + Unix commit time in a single `git log` call
/// (hq-fe-api-r.10 + .11). `--format=%s%n%an%n%ct` yields one record per line so partial
/// parses degrade gracefully: a worktree with no commits or an unreadable HEAD returns
/// `(None, None, None)` and the panel still renders the row without inventing data.
async fn head_commit_meta(
    wt: &std::path::Path,
) -> (Option<String>, Option<String>, Option<u64>) {
    let out = match run_git(wt, &["log", "-1", "--format=%s%n%an%n%ct", "HEAD"]).await {
        Ok(s) => s,
        Err(_) => return (None, None, None),
    };
    let mut lines = out.lines();
    let subject = lines.next().map(str::to_string).filter(|s| !s.is_empty());
    let author = lines.next().map(str::to_string).filter(|s| !s.is_empty());
    let time = lines.next().and_then(|s| s.parse().ok());
    (subject, author, time)
}

async fn collect_dirty(wt: &std::path::Path) -> Result<Vec<DirtyFileDto>, AppError> {
    let raw = run_git(wt, &["status", "--porcelain=v2"]).await?;
    Ok(parse_porcelain_v2(&raw))
}

async fn ahead_behind(wt: &std::path::Path) -> Result<(u32, u32), AppError> {
    // `--left-right --count main...HEAD` returns `<behind>\t<ahead>`. On main itself or when
    // HEAD == main commit, both sides are zero; if `main` is unreachable from this worktree
    // (rare, e.g. shallow clone), we report zeros instead of failing the whole snapshot.
    let out = match run_git(wt, &["rev-list", "--left-right", "--count", "main...HEAD"]).await {
        Ok(s) => s,
        Err(_) => return Ok((0, 0)),
    };
    let mut iter = out.split_whitespace();
    let behind = iter.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let ahead = iter.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    Ok((ahead, behind))
}

async fn run_git(cwd: &std::path::Path, args: &[&str]) -> Result<String, AppError> {
    let out = tokio::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .await
        .map_err(|e| AppError::internal(format!("git spawn: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(AppError::internal(format!(
            "git {:?} failed: {}",
            args, stderr
        )));
    }
    String::from_utf8(out.stdout).map_err(|e| AppError::internal(format!("git utf8: {e}")))
}

#[derive(Debug)]
struct WorktreeEntry {
    path: String,
    head: String,
    branch: Option<String>,
}

/// Parse `git worktree list --porcelain`. Each worktree is a block of `key value` lines
/// separated by a blank line; we care about `worktree <path>`, `HEAD <sha>`, and either
/// `branch refs/heads/<name>` or a bare `detached` marker. Lines we don't recognize are
/// ignored — the format is append-only across git versions, not a closed schema.
fn parse_worktree_list(out: &str) -> Vec<WorktreeEntry> {
    let mut entries = Vec::new();
    let mut path: Option<String> = None;
    let mut head: Option<String> = None;
    let mut branch: Option<String> = None;

    let flush = |path: &mut Option<String>,
                 head: &mut Option<String>,
                 branch: &mut Option<String>,
                 entries: &mut Vec<WorktreeEntry>| {
        if let (Some(p), Some(h)) = (path.take(), head.take()) {
            entries.push(WorktreeEntry {
                path: p,
                head: h,
                branch: branch.take(),
            });
        } else {
            path.take();
            head.take();
            branch.take();
        }
    };

    for line in out.lines() {
        if line.is_empty() {
            flush(&mut path, &mut head, &mut branch, &mut entries);
            continue;
        }
        if let Some(rest) = line.strip_prefix("worktree ") {
            path = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("HEAD ") {
            head = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("branch refs/heads/") {
            branch = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("branch ") {
            // Non-heads ref (rare) — keep the raw ref so the frontend can still render it.
            branch = Some(rest.to_string());
        }
        // `bare`, `detached`, `locked`, `prunable` etc. are flag lines; absence of `branch`
        // implies detached HEAD, which the DTO encodes as `branch: None`.
    }
    flush(&mut path, &mut head, &mut branch, &mut entries);
    entries
}

/// Parse `git status --porcelain=v2`. Each line begins with a sigil identifying the record:
/// `1` tracked, `2` rename/copy, `u` unmerged, `?` untracked, `!` ignored. We expose only the
/// 2-char `xy` code + the worktree-relative path; ignored entries are dropped (the dashboard
/// renders dirty work, not gitignore matches). Rename lines list both new and original paths
/// separated by a tab — we report the new path only, matching what VSCode's SCM panel does.
fn parse_porcelain_v2(out: &str) -> Vec<DirtyFileDto> {
    let mut dirty = Vec::new();
    for line in out.lines() {
        let mut parts = line.splitn(2, ' ');
        let sigil = parts.next().unwrap_or("");
        let rest = parts.next().unwrap_or("");
        match sigil {
            "1" => {
                // `1 XY sub mH mI mW hH hI path`
                let mut fields = rest.splitn(8, ' ');
                let xy = fields.next().unwrap_or("").to_string();
                let path = fields.nth(6).unwrap_or("").to_string();
                if !path.is_empty() {
                    dirty.push(DirtyFileDto { path, xy });
                }
            }
            "2" => {
                // `2 XY sub mH mI mW hH hI Xscore path\torigPath`
                let mut fields = rest.splitn(9, ' ');
                let xy = fields.next().unwrap_or("").to_string();
                let tail = fields.nth(7).unwrap_or("");
                let path = tail.split('\t').next().unwrap_or("").to_string();
                if !path.is_empty() {
                    dirty.push(DirtyFileDto { path, xy });
                }
            }
            "u" => {
                // `u XY sub m1 m2 m3 mW h1 h2 h3 path`
                let mut fields = rest.splitn(10, ' ');
                let xy = fields.next().unwrap_or("").to_string();
                let path = fields.nth(8).unwrap_or("").to_string();
                if !path.is_empty() {
                    dirty.push(DirtyFileDto { path, xy });
                }
            }
            "?" => {
                dirty.push(DirtyFileDto {
                    path: rest.to_string(),
                    xy: "??".to_string(),
                });
            }
            _ => {}
        }
    }
    dirty
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
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
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
