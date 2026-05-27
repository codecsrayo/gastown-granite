//! Patrol actor: single owner of the live-lease tracker. The actor itself never reads the
//! clock — the *edge* (an async tick task in the bin) reads it and passes `now_secs` on
//! every `Heartbeat` / `Tick`. That keeps detection replay-able.

use tokio::sync::{mpsc, oneshot};

use gt_events::Envelope;

use crate::events::PatrolEvent;
use crate::expectations::expired_leases;
use crate::state::LeaseTracker;

pub enum PatrolMsg {
    /// Dispatcher just claimed `bead` for `worker`: start a lease.
    Register {
        bead: String,
        worker: String,
        priority: u8,
        now_secs: u64,
    },
    /// Worker still alive: refresh every lease it owns.
    Heartbeat {
        worker: String,
        now_secs: u64,
    },
    /// Completion/failure observed elsewhere: drop the lease without firing expired.
    Close {
        bead: String,
    },
    /// Edge tick: run the pure detector against `now_secs` and emit `LeaseExpired` for
    /// every stale lease. Removes them from the tracker so a follow-up tick at the same
    /// `now_secs` does not re-fire the same expiration.
    Tick {
        now_secs: u64,
        timeout_secs: u64,
    },
    /// Diagnostics: snapshot `(live_leases, expired_emitted_total)`.
    Snapshot(oneshot::Sender<(usize, usize)>),
}

#[derive(Clone)]
pub struct PatrolHandle {
    tx: mpsc::Sender<PatrolMsg>,
}

impl PatrolHandle {
    pub async fn register(&self, bead: impl Into<String>, worker: impl Into<String>, priority: u8, now_secs: u64) {
        let _ = self
            .tx
            .send(PatrolMsg::Register {
                bead: bead.into(),
                worker: worker.into(),
                priority,
                now_secs,
            })
            .await;
    }

    pub async fn heartbeat(&self, worker: impl Into<String>, now_secs: u64) {
        let _ = self
            .tx
            .send(PatrolMsg::Heartbeat {
                worker: worker.into(),
                now_secs,
            })
            .await;
    }

    pub async fn close(&self, bead: impl Into<String>) {
        let _ = self.tx.send(PatrolMsg::Close { bead: bead.into() }).await;
    }

    pub async fn tick(&self, now_secs: u64, timeout_secs: u64) {
        let _ = self.tx.send(PatrolMsg::Tick { now_secs, timeout_secs }).await;
    }

    pub async fn snapshot(&self) -> (usize, usize) {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(PatrolMsg::Snapshot(reply)).await.is_err() {
            return (0, 0);
        }
        rx.await.unwrap_or((0, 0))
    }
}

/// Spawn the patrol actor. `events` is the relay (`mpsc`) into the sync bus that the
/// composition root drains. Every observation (Register/Heartbeat/Close) is mirrored to
/// the log so replay reconstructs the same tracker state. `LeaseExpired` is emitted only
/// from `Tick`, ordered by bead id for deterministic replay.
pub fn spawn(events: mpsc::Sender<Envelope<PatrolEvent>>) -> PatrolHandle {
    let (tx, mut rx) = mpsc::channel::<PatrolMsg>(64);
    tokio::spawn(async move {
        let mut tracker = LeaseTracker::default();
        let mut expired_emitted: usize = 0;

        while let Some(msg) = rx.recv().await {
            match msg {
                PatrolMsg::Register { bead, worker, priority, now_secs } => {
                    tracker.register(bead.clone(), worker.clone(), priority, now_secs);
                    let _ = events
                        .send(Envelope::root(PatrolEvent::LeaseRegistered {
                            bead,
                            worker,
                            priority,
                            now_secs,
                        }))
                        .await;
                }
                PatrolMsg::Heartbeat { worker, now_secs } => {
                    tracker.heartbeat(&worker, now_secs);
                    let _ = events
                        .send(Envelope::root(PatrolEvent::Heartbeat { worker, now_secs }))
                        .await;
                }
                PatrolMsg::Close { bead } => {
                    tracker.close(&bead);
                    let _ = events
                        .send(Envelope::root(PatrolEvent::LeaseClosed { bead }))
                        .await;
                }
                PatrolMsg::Tick { now_secs, timeout_secs } => {
                    // Pure detection against a sorted snapshot → deterministic emit order.
                    let snap = tracker.snapshot();
                    let stale = expired_leases(&snap, now_secs, timeout_secs);
                    for lease in stale {
                        tracker.close(&lease.bead);
                        expired_emitted += 1;
                        let _ = events
                            .send(Envelope::root(PatrolEvent::LeaseExpired {
                                bead: lease.bead,
                                worker: lease.worker,
                                priority: lease.priority,
                            }))
                            .await;
                    }
                }
                PatrolMsg::Snapshot(reply) => {
                    let _ = reply.send((tracker.len(), expired_emitted));
                }
            }
        }
    });
    PatrolHandle { tx }
}
