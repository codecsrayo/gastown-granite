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
