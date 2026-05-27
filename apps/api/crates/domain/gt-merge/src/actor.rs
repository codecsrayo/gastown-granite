//! Actor del dominio merge: dueño único del `MergeBoard`. Sin `Arc<Mutex>`: los mensajes
//! entran por `mpsc`, las observaciones salen por el relay `events` hacia el bus síncrono
//! del composition root.
//!
//! Los eventos emitidos al log son los que el actor ya *aplicó* al estado interno: si la
//! transición falla, no se publica nada (preserva la determinismo del replay — solo
//! transiciones legales quedan grabadas).

use tokio::sync::{mpsc, oneshot};

use gt_events::Envelope;

use crate::events::MergeEvent;
use crate::state::{MergeBoard, MergeSlot};

pub enum MergeMsg {
    /// Refinery tradujo un `MERGE_READY` del channel. Crea el slot en `Ready`.
    Submit {
        bead: String,
        branch: String,
        channel_msg_id: String,
    },
    /// Composition root pide al actor que avance al merge físico (`Ready → Merging`).
    Start { bead: String },
    /// El borde reporta merge exitoso (`Merging → Merged`).
    Complete { bead: String, sha: String },
    /// El borde reporta fallo (`Merging → Failed`).
    Fail { bead: String, reason: String },
    /// Snapshot diagnóstico del board.
    Snapshot(oneshot::Sender<Vec<MergeSlot>>),
}

#[derive(Clone)]
pub struct MergeHandle {
    tx: mpsc::Sender<MergeMsg>,
}

impl MergeHandle {
    pub async fn submit(
        &self,
        bead: impl Into<String>,
        branch: impl Into<String>,
        channel_msg_id: impl Into<String>,
    ) {
        let _ = self
            .tx
            .send(MergeMsg::Submit {
                bead: bead.into(),
                branch: branch.into(),
                channel_msg_id: channel_msg_id.into(),
            })
            .await;
    }

    pub async fn start(&self, bead: impl Into<String>) {
        let _ = self.tx.send(MergeMsg::Start { bead: bead.into() }).await;
    }

    pub async fn complete(&self, bead: impl Into<String>, sha: impl Into<String>) {
        let _ = self
            .tx
            .send(MergeMsg::Complete {
                bead: bead.into(),
                sha: sha.into(),
            })
            .await;
    }

    pub async fn fail(&self, bead: impl Into<String>, reason: impl Into<String>) {
        let _ = self
            .tx
            .send(MergeMsg::Fail {
                bead: bead.into(),
                reason: reason.into(),
            })
            .await;
    }

    pub async fn snapshot(&self) -> Vec<MergeSlot> {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(MergeMsg::Snapshot(reply)).await.is_err() {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }
}

/// Arranca el actor. `events` es el relay (mpsc) hacia el bus síncrono del composition
/// root, que a su vez lo persiste en el audit log.
pub fn spawn(events: mpsc::Sender<Envelope<MergeEvent>>) -> MergeHandle {
    let (tx, mut rx) = mpsc::channel::<MergeMsg>(64);
    tokio::spawn(async move {
        let mut board = MergeBoard::default();
        while let Some(msg) = rx.recv().await {
            match msg {
                MergeMsg::Submit { bead, branch, channel_msg_id } => {
                    if board.submit(bead.clone(), branch.clone()).is_ok() {
                        let _ = events
                            .send(Envelope::root(MergeEvent::Ready {
                                bead,
                                branch,
                                channel_msg_id,
                            }))
                            .await;
                    }
                }
                MergeMsg::Start { bead } => {
                    if board.start(&bead).is_ok() {
                        let _ = events.send(Envelope::root(MergeEvent::Started { bead })).await;
                    }
                }
                MergeMsg::Complete { bead, sha } => {
                    if board.complete(&bead).is_ok() {
                        let _ = events
                            .send(Envelope::root(MergeEvent::Merged { bead, sha }))
                            .await;
                    }
                }
                MergeMsg::Fail { bead, reason } => {
                    if board.fail(&bead).is_ok() {
                        let _ = events
                            .send(Envelope::root(MergeEvent::Failed { bead, reason }))
                            .await;
                    }
                }
                MergeMsg::Snapshot(reply) => {
                    let _ = reply.send(board.snapshot());
                }
            }
        }
    });
    MergeHandle { tx }
}
