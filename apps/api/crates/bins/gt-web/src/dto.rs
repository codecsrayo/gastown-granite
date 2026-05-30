//! Wire DTOs. The browser frontend never sees domain types directly — DTOs are the stable
//! JSON contract (`docs/07-frontend.md`). Translating here keeps refactors of the domain
//! invisible to clients.

use serde::{Deserialize, Serialize};

use gt_agent::{Session, SessionState};
use gt_beads::{Bead, BeadStatus};

/// One row of `GET /api/sessions`. `role`/`crew` (hq-8iur.7) expose the agent kind and the
/// crew running inside a polecat as the flat canonical strings the frontend can filter on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionDto {
    pub id: String,
    pub rig: String,
    pub state: String,
    pub role: String,
    pub crew: Option<String>,
}

impl From<Session> for SessionDto {
    fn from(s: Session) -> Self {
        Self {
            id: s.id,
            rig: s.rig,
            state: match s.state {
                SessionState::Spawned => "spawned",
                SessionState::Working => "working",
                SessionState::Done => "done",
                SessionState::Killed => "killed",
            }
            .to_string(),
            role: s.role.as_str().to_string(),
            crew: s.crew,
        }
    }
}

/// Query for `GET /api/sessions?role=polecat`. Absent = all active sessions (no role filter).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SessionsQuery {
    pub role: Option<String>,
}

/// One row of `GET /api/beads`. Mirrors the columns the dashboard already reads.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BeadDto {
    pub id: String,
    pub title: String,
    pub status: String,
    pub priority: u8,
    pub assignee: Option<String>,
}

impl From<Bead> for BeadDto {
    fn from(b: Bead) -> Self {
        Self {
            id: b.id,
            title: b.title,
            status: b.status.as_str().to_string(),
            priority: b.priority,
            assignee: b.assignee,
        }
    }
}

/// Query for `GET /api/beads?status=pending`. Default = pending (the operator-visible queue).
#[derive(Debug, Clone, Deserialize)]
pub struct BeadsQuery {
    #[serde(default = "default_status")]
    pub status: String,
}

fn default_status() -> String {
    "pending".to_string()
}

impl BeadsQuery {
    pub fn parsed(&self) -> Option<BeadStatus> {
        BeadStatus::parse(&self.status)
    }
}

/// Body of `POST /api/nudge` — a write-side command, not an event. The handler turns it into
/// an `AgentEvent::Heartbeat` on the agent relay; the actor records it and replay sees it.
#[derive(Debug, Clone, Deserialize)]
pub struct NudgeRequest {
    pub session: String,
}

/// Response of `POST /api/nudge`.
#[derive(Debug, Clone, Serialize)]
pub struct NudgeResponse {
    pub accepted: bool,
}

/// One row of `GET /api/worktrees` (hq-fe-api-r.8). Mirrors what VSCode's SCM panel renders
/// per-repo: the worktree path, its current branch and HEAD, the divergence vs. the rig's
/// default branch (`main` in hq), and the dirty file list. The dashboard joins this against
/// `GET /api/sessions` to label each row with the agent on it (branch convention
/// `claim/<bead-id>` per `apps/web/docs/frontend-features.md`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeDto {
    /// Absolute path of the worktree on disk (matches `git worktree list --porcelain` output).
    pub path: String,
    /// Branch name, or `None` for a detached HEAD (porcelain emits `detached` then).
    pub branch: Option<String>,
    /// 40-char object id at HEAD (porcelain emits `HEAD <sha>`).
    pub head: String,
    /// `true` for the main worktree (porcelain emits `bare` or no `worktree` parent flag).
    pub is_main: bool,
    /// Commits this branch has that `main` does not (right side of `main...HEAD`).
    pub ahead: u32,
    /// Commits `main` has that this branch does not (left side of `main...HEAD`).
    pub behind: u32,
    /// Working-tree changes (porcelain v2 lines), one entry per dirty path. Empty when clean.
    pub dirty: Vec<DirtyFileDto>,
}

/// One dirty path inside a worktree. Mirrors `git status --porcelain=v2` shape — the `xy`
/// 2-char code carries staged (`x`) + unstaged (`y`) state so the frontend can render the
/// same `M`/`U`/`A`/`?` glyphs VSCode does without re-parsing rules client-side.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirtyFileDto {
    /// Path relative to the worktree root (matches porcelain v2 final column).
    pub path: String,
    /// Two-letter status code: index state + worktree state. `??` for untracked, `M.` for
    /// staged modify, `.M` for unstaged modify, `A.` for staged add, `UU` for unmerged.
    pub xy: String,
}

/// One row of `GET /api/issues` (hq-fe-api-r.9). Thin HTTP mirror of the `gt://issues`
/// MCP resource: surfaces the canonical `hq.issues` slice the dashboard joins against
/// (e.g. /worktrees cross-link hq-fe-view.15). Heavy fields (description/design/notes)
/// stay off the listing — clients fetch them per-id when a row is opened.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IssueDto {
    pub id: String,
    pub title: String,
    /// Canonical `open|working|closed` from `hq.issues.status`. Distinct from
    /// `BeadDto.status` (which mirrors the dispatcher's `beads` table).
    pub status: String,
    pub priority: i32,
    pub issue_type: String,
    pub assignee: Option<String>,
    pub owner: Option<String>,
    /// Parent epic id (the bead's `external_ref` column).
    pub external_ref: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub closed_at: Option<String>,
}

/// Query for `GET /api/issues?status=working&external_ref=hq-fe-view&limit=50`. All fields
/// optional; absent = no filter. `status` accepts a comma-separated list so the dashboard
/// can pull `open,working` in one round-trip (mirrors the MCP resource's grammar).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct IssuesQuery {
    pub status: Option<String>,
    pub assignee: Option<String>,
    pub external_ref: Option<String>,
    pub issue_type: Option<String>,
    pub priority_max: Option<u8>,
    pub limit: Option<u32>,
}
