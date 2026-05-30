//! Polecat killer port (hq-fe-api-w.6). `DELETE /api/sessions/:id` is the dashboard's
//! e-stop on a runaway agent: the route looks the session up in the registry, calls the
//! port to terminate the underlying tmux session, and lets the reactor close the
//! lifecycle via `AgentEvent::Killed`. The trait keeps `gt-web` decoupled from the
//! tmux edge — production cables [`TmuxPolecatKiller`] (wrapping `gt_polecat::Tmux`),
//! tests use [`InMemoryPolecatKiller`] to assert the route called us with the right id.
//!
//! The port is intentionally tiny: one method, returns `Result<(), AppError>`. A missing
//! tmux session surfaces as 500 by design — the handler pre-checks the registry, so an
//! IO error here is a real edge failure (`tmux` binary missing, server unreachable),
//! not "the user asked for a session that doesn't exist".

use std::sync::{Arc, Mutex};

use gt_polecat::Tmux;

use crate::routes::AppError;

/// Edge port: terminate the tmux session backing a polecat. Cheap to clone (Arc-backed
/// impls) so the gateway can hand it to handlers via [`crate::AppState`].
pub trait PolecatKiller: Send + Sync {
    /// Send SIGTERM via `tmux kill-session -t <session>`. Idempotent at the route layer:
    /// the handler verifies the session is still in the registry before dispatching, so
    /// double-kill of an already-dead session returns 404 rather than a leaked tmux error.
    fn kill(&self, session: &str) -> Result<(), AppError>;
}

/// Production adapter: forwards to any [`gt_polecat::Tmux`] implementation. The shared
/// `Arc<dyn Tmux>` is the same edge adapter the supervisor watches polecats with, so the
/// kill path can never desync from the live tmux server (no separate client, no socket
/// drift). Wraps the trait object in a struct to keep [`PolecatKiller`] dyn-safe.
pub struct TmuxPolecatKiller {
    tmux: Arc<dyn Tmux>,
}

impl TmuxPolecatKiller {
    pub fn new(tmux: Arc<dyn Tmux>) -> Self {
        Self { tmux }
    }
}

impl PolecatKiller for TmuxPolecatKiller {
    fn kill(&self, session: &str) -> Result<(), AppError> {
        self.tmux
            .kill_session(session)
            .map_err(|e| AppError::internal(format!("tmux kill-session {session}: {e}")))
    }
}

/// Test double: records every kill the handler issued so gates can assert the route
/// called us with the canonical session id (not, say, a tmux-prefixed variant). Always
/// succeeds; tests that need an edge failure mode can wrap a custom impl.
#[derive(Default, Clone)]
pub struct InMemoryPolecatKiller {
    killed: Arc<Mutex<Vec<String>>>,
}

impl InMemoryPolecatKiller {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn killed(&self) -> Vec<String> {
        self.killed.lock().unwrap().clone()
    }
}

impl PolecatKiller for InMemoryPolecatKiller {
    fn kill(&self, session: &str) -> Result<(), AppError> {
        self.killed.lock().unwrap().push(session.to_string());
        Ok(())
    }
}
