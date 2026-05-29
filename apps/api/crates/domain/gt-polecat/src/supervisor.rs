//! Daemon supervision: watch a polecat's heartbeat and restart it with backoff.
//!
//! Port of the live half of `internal/daemon` (the parts that supervise polecats). The pure
//! decision of *whether* to restart lives in [`RestartTracker`]; this module is the async edge
//! that watches a real child, decides death (process exit or stale heartbeat), and drives the
//! (re)spawn loop — emitting [`AgentEvent`]s to the relay so the session projection/replay
//! stays authoritative (the gate: "tracked in sessions + AgentEvent log").
//!
//! Like `gt_agent::supervisor`, it never touches the (sync, `!Send`) bus directly: it pushes
//! envelopes to an `mpsc` the bus-owning task drains.

use std::io;
use std::time::Duration;

use tokio::sync::mpsc;

use gt_agent::AgentEvent;
use gt_events::Envelope;

use crate::lifecycle::{heartbeat_is_stale, spawn_process, SpawnSpec, SpawnedPolecat};
use crate::restart::RestartTracker;

/// Why a watched polecat stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchOutcome {
    /// The process exited on its own — normal completion or an external `kill -9`.
    Exited,
    /// The heartbeat went stale; the supervisor killed the (hung) process.
    StaleKilled,
}

impl WatchOutcome {
    /// The death event to record for this outcome.
    fn end_event(self, session: &str) -> AgentEvent {
        match self {
            WatchOutcome::Exited => AgentEvent::SessionEnd {
                session: session.to_string(),
            },
            WatchOutcome::StaleKilled => AgentEvent::Killed {
                session: session.to_string(),
                reason: "heartbeat stale".to_string(),
            },
        }
    }
}

/// Watch a spawned polecat until it dies. Polls the heartbeat every `poll`; if it is older
/// than `stale_after` the process is presumed hung and killed. Removes the heartbeat file
/// before returning so a re-spawn starts clean.
pub async fn watch(p: &mut SpawnedPolecat, stale_after: Duration, poll: Duration) -> WatchOutcome {
    let hb = p.heartbeat.clone();
    let child = p.child_mut();
    let mut tick = tokio::time::interval(poll);
    let outcome = loop {
        tokio::select! {
            _ = child.wait() => break WatchOutcome::Exited,
            _ = tick.tick() => {
                if heartbeat_is_stale(&hb, stale_after) {
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                    break WatchOutcome::StaleKilled;
                }
            }
        }
    };
    let _ = tokio::fs::remove_file(&hb).await;
    outcome
}

/// Supervision policy for [`supervise_polecat`].
#[derive(Debug, Clone, Copy)]
pub struct RespawnPolicy {
    /// Heartbeat age past which the polecat is presumed hung.
    pub stale_after: Duration,
    /// How often to check the heartbeat.
    pub poll: Duration,
    /// Hard cap on re-spawns before giving up (separate from the crash-loop budget). Use a
    /// large value for an effectively-unbounded production supervisor; small for tests.
    pub max_restarts: u32,
}

impl Default for RespawnPolicy {
    fn default() -> Self {
        Self {
            stale_after: Duration::from_secs(90),
            poll: Duration::from_secs(1),
            max_restarts: u32::MAX,
        }
    }
}

/// Run the spawn → watch → restart loop for one polecat.
///
/// `make_spec` produces a fresh [`SpawnSpec`] for each (re)spawn (so a re-spawn can pick a new
/// run id / refresh env). `tracker` gates restarts with backoff + crash-loop detection;
/// `now_fn` injects unix-seconds at the edge. Stops when the restart budget is exhausted, the
/// tracker refuses (crash loop), or `max_restarts` is hit.
pub async fn supervise_polecat<F, N>(
    agent_id: &str,
    mut make_spec: F,
    tracker: &mut RestartTracker,
    policy: RespawnPolicy,
    events: mpsc::Sender<Envelope<AgentEvent>>,
    now_fn: N,
) -> io::Result<()>
where
    F: FnMut() -> SpawnSpec,
    N: Fn() -> u64,
{
    let mut restarts = 0u32;
    loop {
        let spec = make_spec();
        let session = spec.session.clone();
        let mut p = spawn_process(&spec).await?;
        let _ = events.send(spec.spawned_envelope()).await;

        let outcome = watch(&mut p, policy.stale_after, policy.poll).await;
        let _ = events
            .send(Envelope::root(outcome.end_event(&session)))
            .await;

        if restarts >= policy.max_restarts {
            break;
        }
        let now = now_fn();
        if !tracker.can_restart(agent_id, now) {
            break;
        }
        tracker.record_restart(agent_id, now);
        let backoff = tracker.backoff_remaining(agent_id, now);
        if backoff > 0 {
            tokio::time::sleep(Duration::from_secs(backoff)).await;
        }
        restarts += 1;
    }
    Ok(())
}
