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

/// Run a long-running daemon loop under restart + backoff supervision — the generic sibling of
/// [`supervise_polecat`] for the in-process role daemons (refinery channel watcher, witness
/// patrol tick, mayor orch loop, …) the composition root boots (hq-mc72.12 C2).
///
/// `run` produces the daemon future for each (re)start; when it resolves — the loop returned
/// (channel abandoned) or crashed — the same [`RestartTracker`] that gates polecats decides
/// whether to restart and how long to back off. Stops on crash-loop, when the tracker refuses,
/// or when `max_restarts` is hit (use `u32::MAX` for an effectively-unbounded daemon). `name`
/// keys the restart bookkeeping; `now_fn` injects unix seconds at the edge.
pub async fn supervise_daemon<F, Fut, N>(
    name: &str,
    mut run: F,
    tracker: &mut RestartTracker,
    max_restarts: u32,
    now_fn: N,
) where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = ()>,
    N: Fn() -> u64,
{
    let mut restarts = 0u32;
    loop {
        run().await;
        if restarts >= max_restarts {
            break;
        }
        let now = now_fn();
        if !tracker.can_restart(name, now) {
            break;
        }
        tracker.record_restart(name, now);
        let backoff = tracker.backoff_remaining(name, now);
        if backoff > 0 {
            tokio::time::sleep(Duration::from_secs(backoff)).await;
        }
        restarts += 1;
    }
}

#[cfg(test)]
mod daemon_tests {
    use super::*;
    use crate::restart::RestartConfig;
    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
    use std::sync::Arc;

    /// A `Fn() -> u64` clock that advances 1000s per call so each restart's backoff window has
    /// elapsed by the next `can_restart` check (otherwise a fixed `now` parks the loop in its
    /// own backoff). `fetch_add` returns the pre-increment value: 0, 1000, 2000, …
    fn advancing_clock() -> impl Fn() -> u64 {
        let now = Arc::new(AtomicU64::new(0));
        move || now.fetch_add(1000, Ordering::SeqCst)
    }

    #[tokio::test(start_paused = true)]
    async fn supervise_daemon_restarts_until_max() {
        // A daemon whose loop returns immediately drives the restart loop fast; paused time
        // makes the backoff sleeps auto-advance so the test stays instant.
        let runs = Arc::new(AtomicU32::new(0));
        let r = runs.clone();
        let mut tracker = RestartTracker::new(RestartConfig {
            initial_backoff_secs: 1,
            crash_loop_count: 100,
            ..RestartConfig::default()
        });
        supervise_daemon(
            "d",
            move || {
                let r = r.clone();
                async move {
                    r.fetch_add(1, Ordering::SeqCst);
                }
            },
            &mut tracker,
            2,
            advancing_clock(),
        )
        .await;
        // initial run + 2 restarts.
        assert_eq!(runs.load(Ordering::SeqCst), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn supervise_daemon_stops_on_crash_loop() {
        let runs = Arc::new(AtomicU32::new(0));
        let r = runs.clone();
        // crash_loop_count=2: once 2 restarts land inside the window the tracker refuses, so
        // the loop stops well before max_restarts.
        let mut tracker = RestartTracker::new(RestartConfig {
            initial_backoff_secs: 1,
            crash_loop_count: 2,
            ..RestartConfig::default()
        });
        supervise_daemon(
            "d",
            move || {
                let r = r.clone();
                async move {
                    r.fetch_add(1, Ordering::SeqCst);
                }
            },
            &mut tracker,
            u32::MAX,
            advancing_clock(),
        )
        .await;
        // initial + 2 restarts, then crash-loop blocks further restarts.
        assert_eq!(runs.load(Ordering::SeqCst), 3);
    }
}
