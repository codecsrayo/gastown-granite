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

use gt_polecat::{PolecatLifecycle, Tmux, GT_HOOK_BEAD};

use crate::routes::AppError;

/// Result of a successful respawn (hq-fe-api-w.7). The session id is the tmux session
/// the lifecycle spawned — equal to the pre-restart id because `SpawnTemplate::spec_for`
/// derives the session name from the (sanitized) `member`, which the respawner reads
/// back from the dying session's `GT_HOOK_BEAD` env. Rig + convoy mirror the spec the
/// lifecycle produced so the handler can rebuild a fresh `AgentEvent::Spawned`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RespawnInfo {
    pub session: String,
    pub rig: String,
    pub member: String,
    pub convoy: Option<String>,
}

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

/// Edge port: cold-restart a live polecat (hq-fe-api-w.7). Reads the dying session's
/// `GT_HOOK_BEAD` and `GT_CONVOY` env (so the operator does not need to re-supply
/// them), tears the tmux session down, then slings a fresh polecat with the same
/// hook. Returns [`RespawnInfo`] so the handler can publish the matching
/// `AgentEvent::Killed` + `AgentEvent::Spawned` envelopes the in-process reactor
/// would have emitted.
pub trait PolecatRespawner: Send + Sync {
    fn respawn(&self, session: &str) -> Result<RespawnInfo, AppError>;
}

/// Production adapter: drives the same [`PolecatLifecycle`] the composition root uses
/// for `sling`. Sharing the lifecycle means the new polecat carries identical env, work
/// directory, and `GT_HOOK_BEAD` pinning as the original — restart is a "fresh process
/// in the same harness", not a re-derived spawn.
pub struct LifecyclePolecatRespawner {
    tmux: Arc<dyn Tmux>,
    lifecycle: Arc<PolecatLifecycle>,
}

impl LifecyclePolecatRespawner {
    pub fn new(tmux: Arc<dyn Tmux>, lifecycle: Arc<PolecatLifecycle>) -> Self {
        Self { tmux, lifecycle }
    }
}

impl PolecatRespawner for LifecyclePolecatRespawner {
    fn respawn(&self, session: &str) -> Result<RespawnInfo, AppError> {
        // Read the hook + convoy pins *before* killing — once the tmux session is gone
        // we lose the env, and reconstructing it from env-vars on the gateway would skip
        // any operator-set per-session overrides.
        let member = self
            .tmux
            .show_environment(session, GT_HOOK_BEAD)
            .map_err(|e| AppError::internal(format!("tmux show-environment {session}: {e}")))?
            .ok_or_else(|| {
                AppError::bad_request(format!(
                    "session {session} has no {GT_HOOK_BEAD} env — cannot restart \
                     (operator must spawn a fresh polecat via convoy launch)"
                ))
            })?;
        let convoy = self
            .tmux
            .show_environment(session, "GT_CONVOY")
            .map_err(|e| AppError::internal(format!("tmux show-environment {session}: {e}")))?
            .unwrap_or_default();
        let convoy_arg = if convoy.is_empty() { "_" } else { &convoy };

        self.tmux
            .kill_session(session)
            .map_err(|e| AppError::internal(format!("tmux kill-session {session}: {e}")))?;
        let spec = self
            .lifecycle
            .sling(convoy_arg, &member)
            .map_err(|e| AppError::internal(format!("polecat sling: {e}")))?;
        Ok(RespawnInfo {
            session: spec.session,
            rig: spec.rig,
            member: spec.polecat,
            convoy: if convoy.is_empty() { None } else { Some(convoy) },
        })
    }
}

/// Test double for [`PolecatRespawner`]. Records every restart the handler issued and
/// returns a canned [`RespawnInfo`] so gates can assert the route called us with the
/// right session id and consumed the response correctly.
#[derive(Default, Clone)]
pub struct InMemoryPolecatRespawner {
    restarts: Arc<Mutex<Vec<String>>>,
    info: Arc<Mutex<RespawnInfo>>,
}

impl InMemoryPolecatRespawner {
    pub fn new(info: RespawnInfo) -> Self {
        Self {
            restarts: Arc::new(Mutex::new(Vec::new())),
            info: Arc::new(Mutex::new(info)),
        }
    }

    pub fn restarts(&self) -> Vec<String> {
        self.restarts.lock().unwrap().clone()
    }
}

impl PolecatRespawner for InMemoryPolecatRespawner {
    fn respawn(&self, session: &str) -> Result<RespawnInfo, AppError> {
        self.restarts.lock().unwrap().push(session.to_string());
        Ok(self.info.lock().unwrap().clone())
    }
}

impl Default for RespawnInfo {
    fn default() -> Self {
        Self {
            session: String::new(),
            rig: String::new(),
            member: String::new(),
            convoy: None,
        }
    }
}
