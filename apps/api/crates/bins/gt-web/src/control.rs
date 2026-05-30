//! Polecat control port (hq-fe-api-w.6 + .8). Surfaces operator e-stop ops on a running
//! polecat — terminate via tmux `kill-session` (the `DELETE /api/sessions/:id` route) and
//! interrupt via tmux `send-keys` (`POST /api/sessions/:id/interrupt`, an `Escape` chord
//! that cancels the agent's in-flight turn without ending the polecat). The trait keeps
//! `gt-web` decoupled from the tmux edge — production cables [`TmuxPolecatControl`]
//! (wrapping `gt_polecat::Tmux`), tests use [`InMemoryPolecatControl`] to assert the
//! route reached the edge with the canonical session id + key chord.
//!
//! Methods are tiny: each returns `Result<(), AppError>`. A missing tmux session
//! surfaces as 500 by design — handlers pre-check the registry, so an IO error here is
//! a real edge failure (`tmux` binary missing, server unreachable), not "the user asked
//! for a session that doesn't exist".

use std::sync::{Arc, Mutex};

use gt_polecat::Tmux;

use crate::routes::AppError;

/// Edge port: control a live polecat's tmux session. Cheap to clone (Arc-backed impls)
/// so the gateway can hand it to handlers via [`crate::AppState`].
pub trait PolecatControl: Send + Sync {
    /// Terminate the tmux session backing a polecat (`tmux kill-session -t <session>`).
    /// Idempotent at the route layer: the handler verifies the session is still in the
    /// registry before dispatching, so double-kill returns 404, not a leaked tmux error.
    fn kill(&self, session: &str) -> Result<(), AppError>;

    /// Send a key chord to the polecat's active pane (`tmux send-keys -t <session> ...`).
    /// `keys` is the verbatim tmux argument list (e.g. `&["Escape"]`, `&["C-c"]`); the
    /// adapter does not translate literals. The interrupt route uses `Escape` to cancel
    /// the agent's current turn without killing the polecat — `kill` is the harder e-stop.
    fn send_keys(&self, session: &str, keys: &[&str]) -> Result<(), AppError>;
}

/// Production adapter: forwards to any [`gt_polecat::Tmux`] implementation. The shared
/// `Arc<dyn Tmux>` is the same edge adapter the supervisor watches polecats with, so the
/// control path can never desync from the live tmux server (no separate client, no
/// socket drift). Wraps the trait object in a struct to keep [`PolecatControl`] dyn-safe.
pub struct TmuxPolecatControl {
    tmux: Arc<dyn Tmux>,
}

impl TmuxPolecatControl {
    pub fn new(tmux: Arc<dyn Tmux>) -> Self {
        Self { tmux }
    }
}

impl PolecatControl for TmuxPolecatControl {
    fn kill(&self, session: &str) -> Result<(), AppError> {
        self.tmux
            .kill_session(session)
            .map_err(|e| AppError::internal(format!("tmux kill-session {session}: {e}")))
    }

    fn send_keys(&self, session: &str, keys: &[&str]) -> Result<(), AppError> {
        self.tmux
            .send_keys(session, keys)
            .map_err(|e| AppError::internal(format!("tmux send-keys {session}: {e}")))
    }
}

/// Test double: records every control op the handler issued so gates can assert the
/// route called us with the canonical session id (and, for interrupt, the right chord).
/// Always succeeds; tests that need an edge failure mode can wrap a custom impl.
#[derive(Default, Clone)]
pub struct InMemoryPolecatControl {
    killed: Arc<Mutex<Vec<String>>>,
    keys_sent: Arc<Mutex<Vec<(String, Vec<String>)>>>,
}

impl InMemoryPolecatControl {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn killed(&self) -> Vec<String> {
        self.killed.lock().unwrap().clone()
    }

    pub fn keys_sent(&self) -> Vec<(String, Vec<String>)> {
        self.keys_sent.lock().unwrap().clone()
    }
}

impl PolecatControl for InMemoryPolecatControl {
    fn kill(&self, session: &str) -> Result<(), AppError> {
        self.killed.lock().unwrap().push(session.to_string());
        Ok(())
    }

    fn send_keys(&self, session: &str, keys: &[&str]) -> Result<(), AppError> {
        let owned: Vec<String> = keys.iter().map(|k| (*k).to_string()).collect();
        self.keys_sent.lock().unwrap().push((session.to_string(), owned));
        Ok(())
    }
}
