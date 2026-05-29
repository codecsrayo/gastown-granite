//! Actor scaffolding (hq-92z9 paso 1). Real message-handling loop + emit-on-apply
//! invariant land in the per-role behavior commit; this skeleton only exposes the
//! handle shape the composition root needs to wire other crates ahead of time.

use tokio::sync::mpsc;

use gt_events::Envelope;

use crate::events::WitnessEvent;

/// Messages accepted by the Witness actor. Scaffolding placeholder until behavior lands.
#[derive(Debug)]
#[non_exhaustive]
pub enum WitnessMsg {
    Placeholder,
}

/// Cloneable handle to the Witness actor. Producers and the composition root send
/// messages via [`WitnessHandle::send`].
#[derive(Clone)]
pub struct WitnessHandle {
    tx: mpsc::Sender<WitnessMsg>,
}

impl WitnessHandle {
    pub async fn send(&self, msg: WitnessMsg) {
        // Best-effort: dropping a message if the actor is gone matches the gt-merge pattern.
        let _ = self.tx.send(msg).await;
    }
}

/// Spawn the Witness actor. **Scaffolding:** the loop drains messages without reacting
/// and never emits to the relay; the behavior commit replaces this body.
pub fn spawn(_events: mpsc::Sender<Envelope<WitnessEvent>>) -> WitnessHandle {
    let (tx, mut rx) = mpsc::channel::<WitnessMsg>(64);
    tokio::spawn(async move {
        while let Some(_msg) = rx.recv().await {
            // TODO(hq-92z9 fill): dispatch messages against WitnessBoard + emit events.
        }
    });
    WitnessHandle { tx }
}
