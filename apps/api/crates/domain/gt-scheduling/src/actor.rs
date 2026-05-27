//! Dispatcher serializado: actor único dueño del dequeue. Serializa el claim dentro del
//! proceso; el CAS es la red para crash/recuperación o un segundo dispatcher.

use tokio::sync::{mpsc, oneshot};

use gt_beads::BeadRepository;
use gt_events::Envelope;

use crate::events::SchedEvent;
use crate::state::{CapacityGovernor, Queue};

pub enum SchedMsg {
    Enqueue { bead: String, priority: u8 },
    /// Un worker terminó: libera capacidad y re-bombea.
    CapacityFreed,
    /// (queued, in_flight)
    Snapshot(oneshot::Sender<(usize, usize)>),
}

#[derive(Clone)]
pub struct SchedHandle {
    tx: mpsc::Sender<SchedMsg>,
}

impl SchedHandle {
    pub async fn enqueue(&self, bead: impl Into<String>, priority: u8) {
        let _ = self
            .tx
            .send(SchedMsg::Enqueue {
                bead: bead.into(),
                priority,
            })
            .await;
    }

    pub async fn capacity_freed(&self) {
        let _ = self.tx.send(SchedMsg::CapacityFreed).await;
    }

    pub async fn snapshot(&self) -> (usize, usize) {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(SchedMsg::Snapshot(reply)).await.is_err() {
            return (0, 0);
        }
        rx.await.unwrap_or((0, 0))
    }
}

/// Arranca el dispatcher. `events` es el relay (mpsc) hacia el bus síncrono del composition
/// root. `max` = capacidad de polecats concurrentes.
pub fn spawn<R>(repo: R, events: mpsc::Sender<Envelope<SchedEvent>>, max: usize) -> SchedHandle
where
    R: BeadRepository + Send + Sync + 'static,
{
    let (tx, mut rx) = mpsc::channel::<SchedMsg>(64);
    tokio::spawn(async move {
        let mut queue = Queue::default();
        let mut gov = CapacityGovernor::new(max);
        let mut worker_seq: u64 = 0;

        while let Some(msg) = rx.recv().await {
            match msg {
                SchedMsg::Enqueue { bead, priority } => {
                    queue.enqueue(bead.clone(), priority);
                    let _ = events
                        .send(Envelope::root(SchedEvent::Enqueue { bead, priority }))
                        .await;
                }
                SchedMsg::CapacityFreed => gov.release(),
                SchedMsg::Snapshot(reply) => {
                    let _ = reply.send((queue.len(), gov.in_flight()));
                    continue; // no re-bombear en una consulta
                }
            }

            // Bombeo: desencola hasta agotar capacidad.
            while gov.can_dispatch() {
                let Some(bead) = queue.pop_highest() else {
                    break;
                };
                worker_seq += 1;
                let worker = format!("w{worker_seq}");
                let ev = match repo.cas_claim(&bead, &worker).await {
                    Ok(true) => {
                        gov.acquire();
                        SchedEvent::Dispatched { bead, worker }
                    }
                    Ok(false) => SchedEvent::DispatchFailed {
                        bead,
                        reason: "no longer pending".into(),
                    },
                    Err(e) => SchedEvent::DispatchFailed {
                        bead,
                        reason: e.to_string(),
                    },
                };
                let _ = events.send(Envelope::root(ev)).await;
            }
        }
    });
    SchedHandle { tx }
}
