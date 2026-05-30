//! Shared state carried by every handler. Generic over the ports so the bin plugs in real
//! adapters and tests plug in in-memory ones; `Arc`s give cheap clones for axum's State.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{broadcast, mpsc};

use gt_agent::{AgentEvent, SessionQueries};
use gt_audit::EventRecord;
use gt_beads::BeadRepository;
use gt_events::Envelope;
use gt_merge::MergeRepository;
use gt_root::CommandBus;
use gt_store_dolt::DoltIssues;

use crate::comments::IssueCommenter;
use crate::control::{PolecatControl, PolecatRespawner};
use crate::dto::WorktreeDto;
use crate::login::{LoginConfig, LoginRegistry};
use gt_terminal::Attach;

/// Read-side composition root. Keep it `Clone` (Arc-backed) so axum can hand a fresh handle
/// to each request without locks. `R`/`SQ`/`M` are the bead / session / merge ports.
pub struct AppState<R, SQ, M>
where
    R: BeadRepository + Send + Sync + 'static,
    SQ: SessionQueries + Send + Sync + 'static,
    M: MergeRepository + Send + Sync + 'static,
{
    pub beads: Arc<R>,
    pub sessions: Arc<SQ>,
    /// `gt_merge::MergeRepository` reader. Powers `GET /api/merges` (hq-fe-api-r.4):
    /// snapshot of the merge slot board (one row per bead in the queue, with its
    /// current state). Production cables Dolt; tests use `InMemoryMergeRepo`.
    pub merges: Arc<M>,
    pub agent_events: mpsc::Sender<Envelope<AgentEvent>>,
    pub events: broadcast::Sender<EventRecord>,
    /// Town root absolute path. `GET /api/worktrees` shells `git -C <root>` to enumerate
    /// worktrees the agents have branched off (hq-fe-api-r.8). Optional: when unset the
    /// endpoint reports an empty list — the gateway does not invent a default rig location.
    pub town_root: Option<Arc<PathBuf>>,
    /// Dolt-backed `hq.issues` reader. Powers `GET /api/issues` (hq-fe-api-r.9), the
    /// canonical 25-col bead table distinct from `beads` (5-col dispatcher scratch). Optional:
    /// when `GT_DOLT_URL` is unset the endpoint returns an empty list so the in-memory dev
    /// mode keeps working without a Dolt connection. Concrete type by design — `DoltIssues`
    /// is the only reader; introducing a port would add a generic for no current consumer.
    pub issues: Option<Arc<DoltIssues>>,
    /// `gt-root::CommandBus` clone (hq-fe-api-w.10). Write-side HTTP routes dispatch
    /// through here instead of carrying per-domain handles; first consumer is
    /// `POST /api/quota/accounts/:n/{rotate,retire}`. `None` in test setups that build
    /// AppState without a live `RootHandle`; those tests exercise routes that don't need
    /// the bus.
    pub bus: Option<CommandBus>,
    /// Live worktree-snapshot broadcast (hq-fe-api-r.12). The bin spawns a single polling
    /// task per process when `town_root` is set; that task shells `git` every 2s and sends
    /// the snapshot into this channel. `GET /api/worktrees/stream` subscribes per
    /// connection. `None` when there is no town root configured — the SSE endpoint then
    /// short-circuits, mirroring the `/api/worktrees` empty-list posture.
    pub worktrees_stream: Option<broadcast::Sender<Vec<WorktreeDto>>>,
    /// Polecat control port (hq-fe-api-w.6 + .8). Backs `DELETE /api/sessions/:id`
    /// (tmux kill-session) and `POST /api/sessions/:id/interrupt` (tmux send-keys).
    /// Production cables [`crate::TmuxPolecatControl`] over a shared
    /// [`gt_polecat::TmuxCli`] handle so every control op targets the same tmux server
    /// the supervisor already watches. `None` in test setups that do not exercise these
    /// routes — the handlers return 500 ("polecat control not wired") if invoked,
    /// mirroring the posture for `bus`/`issues`.
    pub control: Option<Arc<dyn PolecatControl>>,
    /// Polecat respawner (hq-fe-api-w.7). Backs `POST /api/sessions/:id/restart`.
    /// Production cables [`crate::LifecyclePolecatRespawner`] over a shared
    /// [`gt_polecat::PolecatLifecycle`] so the new polecat carries identical env and
    /// `GT_HOOK_BEAD` pinning as the original — restart is a "fresh process in the
    /// same harness". `None` in test setups not exercising this route; the handler
    /// returns 500 ("polecat respawner not wired") if invoked.
    pub respawner: Option<Arc<dyn PolecatRespawner>>,
    /// Issue commenter port (hq-fe-api-w.5). Backs `POST /api/beads/:id/comments`.
    /// Production cables [`crate::DoltIssueCommenter`] over the same
    /// [`gt_store_dolt::DoltIssues`] handle `GET /api/issues` reads. `None` in
    /// test setups not exercising this route; the handler returns 500
    /// ("issue commenter not wired") if invoked, mirroring the posture for the
    /// other gateway ports.
    pub commenter: Option<Arc<dyn IssueCommenter>>,
    /// Absolute path to the shared `events.jsonl` log (hq-fe-api-r.5). Same file the
    /// reactor + frontier audit append to and the SSE `/api/stream` mirrors. Backs
    /// `GET /api/feed?since=…` historical replay. `None` in test setups not exercising
    /// the feed route; the handler returns `[]` so the dashboard fall-back (live SSE only)
    /// matches the gateway empty-list posture used elsewhere.
    pub event_log: Option<Arc<PathBuf>>,
    /// Account-login flow registry (hq-fe-auth.2). Holds one in-flight
    /// [`crate::login::FlowSlot`] per account so `/login/token` and `/login/cancel` can
    /// route to the right pending `LoginDriver` task. Always present (built at boot); a
    /// missing PTY port short-circuits `start` with 503 instead of hiding the registry.
    pub login_registry: Arc<LoginRegistry>,
    /// PTY adapter the login registry spawns `claude /login` under (hq-fe-auth.2).
    /// Production cables [`gt_login::PortablePty`]; tests cable
    /// [`gt_login::FakePty`] with a scripted output script. `None` disables the route —
    /// `POST /api/quota/accounts/:n/login` returns 503 so a deploy without a `claude`
    /// binary or without GT_LOGIN_ENABLE never silently no-ops.
    pub login_pty: Option<Arc<dyn gt_login::Pty>>,
    /// Shared login command config (program + args + account env var name). Read at
    /// boot from `GT_LOGIN_CMD` / `GT_LOGIN_ARGS`; defaults to `claude /login`.
    pub login_config: Arc<LoginConfig>,
    /// Terminal attach adapter (hq-fe-term.2). Backs `GET /api/sessions/:id/term`
    /// (WebSocket upgrade). Production wires [`gt_terminal::TmuxPipeAttach`] over
    /// [`gt_terminal::CliTmuxAttachOps`]; tests wire [`gt_terminal::FakeAttach`].
    /// `None` (default in deploys without `GT_TERMINAL_ENABLE=1`) makes the route
    /// short-circuit with `503` instead of attempting a `tmux pipe-pane` call on a
    /// host that has no tmux server — same posture as [`Self::login_pty`].
    pub terminal_attach: Option<Arc<dyn Attach>>,
}

impl<R, SQ, M> Clone for AppState<R, SQ, M>
where
    R: BeadRepository + Send + Sync + 'static,
    SQ: SessionQueries + Send + Sync + 'static,
    M: MergeRepository + Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        Self {
            beads: self.beads.clone(),
            sessions: self.sessions.clone(),
            merges: self.merges.clone(),
            agent_events: self.agent_events.clone(),
            events: self.events.clone(),
            town_root: self.town_root.clone(),
            issues: self.issues.clone(),
            bus: self.bus.clone(),
            worktrees_stream: self.worktrees_stream.clone(),
            control: self.control.clone(),
            respawner: self.respawner.clone(),
            commenter: self.commenter.clone(),
            event_log: self.event_log.clone(),
            login_registry: self.login_registry.clone(),
            login_pty: self.login_pty.clone(),
            login_config: self.login_config.clone(),
            terminal_attach: self.terminal_attach.clone(),
        }
    }
}
