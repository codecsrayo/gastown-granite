//! Orchestration actor: single owner of the live `ConvoyBoard`. It never reads the clock —
//! a convoy advances on *facts* (a member finished), which keeps it replay-able. The actor
//! is also the one place that turns a completion into the next handoff: on `MemberDone` it
//! either feeds the next member (`MemberDispatched`) or, if all are done, closes the convoy
//! (`ConvoyClosed`). The real I/O (`gt sling` of the dispatched member, closing the convoy
//! bead) lives at the composition-root edge reacting to those emitted events — same pattern
//! as `gt-merge` (Paso 6.b) and `gt-patrol` (Paso 6.a).

use tokio::sync::{mpsc, oneshot};

use gt_events::Envelope;

use crate::events::OrchEvent;
use crate::state::{Convoy, ConvoyBoard};

pub enum OrchMsg {
    /// Plan a convoy: an ordered set of member beads. Starts `Staged`.
    CreateConvoy {
        convoy: String,
        members: Vec<String>,
    },
    /// Release the convoy (mayor/deacon): launch it and feed the first member.
    Launch {
        convoy: String,
    },
    /// A member's bead finished (observed at the edge): advance the convoy — feed the next
    /// member, or close the convoy if this was the last one.
    MemberDone {
        convoy: String,
        member: String,
    },
    /// A member's bead failed: record it and halt the convoy.
    MemberFail {
        convoy: String,
        member: String,
        reason: String,
    },
    /// Diagnostics: deterministic snapshot of the live board (sorted by convoy id).
    Snapshot(oneshot::Sender<Vec<Convoy>>),
}

#[derive(Clone)]
pub struct OrchHandle {
    tx: mpsc::Sender<OrchMsg>,
}

impl OrchHandle {
    pub async fn create_convoy(&self, convoy: impl Into<String>, members: Vec<String>) {
        let _ = self
            .tx
            .send(OrchMsg::CreateConvoy { convoy: convoy.into(), members })
            .await;
    }

    pub async fn launch(&self, convoy: impl Into<String>) {
        let _ = self.tx.send(OrchMsg::Launch { convoy: convoy.into() }).await;
    }

    pub async fn member_done(&self, convoy: impl Into<String>, member: impl Into<String>) {
        let _ = self
            .tx
            .send(OrchMsg::MemberDone { convoy: convoy.into(), member: member.into() })
            .await;
    }

    pub async fn member_fail(
        &self,
        convoy: impl Into<String>,
        member: impl Into<String>,
        reason: impl Into<String>,
    ) {
        let _ = self
            .tx
            .send(OrchMsg::MemberFail {
                convoy: convoy.into(),
                member: member.into(),
                reason: reason.into(),
            })
            .await;
    }

    pub async fn snapshot(&self) -> Vec<Convoy> {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(OrchMsg::Snapshot(reply)).await.is_err() {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }
}

/// Spawn the orchestration actor. `events` is the relay (`mpsc`) into the sync bus that the
/// composition root drains. Every state change is mirrored to the log so replay
/// reconstructs the same board; emission order is deterministic (one event per fact, in
/// message order) so the gate stays byte-identical.
pub fn spawn(events: mpsc::Sender<Envelope<OrchEvent>>) -> OrchHandle {
    let (tx, mut rx) = mpsc::channel::<OrchMsg>(64);
    tokio::spawn(async move {
        let mut board = ConvoyBoard::default();

        while let Some(msg) = rx.recv().await {
            match msg {
                OrchMsg::CreateConvoy { convoy, members } => {
                    if board.create(convoy.clone(), members.clone()).is_ok() {
                        let _ = events
                            .send(Envelope::root(OrchEvent::ConvoyCreated { convoy, members }))
                            .await;
                    }
                }
                OrchMsg::Launch { convoy } => {
                    if board.launch(&convoy).is_ok() {
                        let _ = events
                            .send(Envelope::root(OrchEvent::ConvoyLaunched {
                                convoy: convoy.clone(),
                            }))
                            .await;
                        feed_or_close(&mut board, &convoy, &events).await;
                    }
                }
                OrchMsg::MemberDone { convoy, member } => {
                    if board.complete(&convoy, &member).is_ok() {
                        let _ = events
                            .send(Envelope::root(OrchEvent::MemberCompleted {
                                convoy: convoy.clone(),
                                member,
                            }))
                            .await;
                        feed_or_close(&mut board, &convoy, &events).await;
                    }
                }
                OrchMsg::MemberFail { convoy, member, reason } => {
                    if board.fail(&convoy, &member).is_ok() {
                        let _ = events
                            .send(Envelope::root(OrchEvent::MemberFailed {
                                convoy: convoy.clone(),
                                member: member.clone(),
                                reason: reason.clone(),
                            }))
                            .await;
                        if board.mark_failed(&convoy).is_ok() {
                            let _ = events
                                .send(Envelope::root(OrchEvent::ConvoyFailed {
                                    convoy,
                                    member,
                                    reason,
                                }))
                                .await;
                        }
                    }
                }
                OrchMsg::Snapshot(reply) => {
                    let _ = reply.send(board.snapshot());
                }
            }
        }
    });
    OrchHandle { tx }
}

/// Advance a launched convoy: if all members are done, close it; otherwise feed the next
/// pending member (the handoff). At most one event is emitted — the convoy is sequential.
async fn feed_or_close(
    board: &mut ConvoyBoard,
    convoy: &str,
    events: &mpsc::Sender<Envelope<OrchEvent>>,
) {
    if board.all_done(convoy) {
        if board.close(convoy).is_ok() {
            let _ = events
                .send(Envelope::root(OrchEvent::ConvoyClosed { convoy: convoy.into() }))
                .await;
        }
    } else if let Some(member) = board.dispatch_next(convoy) {
        let _ = events
            .send(Envelope::root(OrchEvent::MemberDispatched {
                convoy: convoy.into(),
                member,
            }))
            .await;
    }
}
