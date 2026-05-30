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

use crate::dto::{
    BeadDto, BeadsQuery, DirtyFileDto, NudgeRequest, NudgeResponse, SessionDto, SessionsQuery,
    WorktreeDto,
};
use crate::state::AppState;
use crate::stream::sse_from_receiver;

/// `GET /api/sessions[?role=polecat]` — snapshot of active sessions, optionally filtered by
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
    let dtos = rows.into_iter().map(SessionDto::from);
    let filtered: Vec<SessionDto> = match q.role {
        Some(role) => dtos.filter(|d| d.role == role).collect(),
        None => dtos.collect(),
    };
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

async fn collect_worktrees(root: &std::path::Path) -> Result<Vec<WorktreeDto>, AppError> {
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

        out.push(WorktreeDto {
            path: entry.path,
            branch: entry.branch,
            head: entry.head,
            is_main,
            ahead,
            behind,
            dirty,
        });
    }
    Ok(out)
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
