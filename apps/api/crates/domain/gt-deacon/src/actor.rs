//! Actor scaffolding (hq-92z9 paso 1). Real message-handling loop + emit-on-apply
//! invariant land in the per-role behavior commit; this skeleton only exposes the
//! handle shape the composition root needs to wire other crates ahead of time.

use tokio::sync::mpsc;

use gt_events::Envelope;

use crate::events::DeaconEvent;

/// Messages accepted by the Deacon actor. Scaffolding placeholder until behavior lands.
#[derive(Debug)]
#[non_exhaustive]
pub enum DeaconMsg {
    Placeholder,
}

/// Cloneable handle to the Deacon actor. Producers and the composition root send
/// messages via [`DeaconHandle::send`].
#[derive(Clone)]
pub struct DeaconHandle {
    tx: mpsc::Sender<DeaconMsg>,
}

impl DeaconHandle {
    pub async fn send(&self, msg: DeaconMsg) {
        // Best-effort: dropping a message if the actor is gone matches the gt-merge pattern.
        let _ = self.tx.send(msg).await;
    }
}

/// Spawn the Deacon actor. **Scaffolding:** the loop drains messages without reacting
/// and never emits to the relay; the behavior commit replaces this body.
pub fn spawn(_events: mpsc::Sender<Envelope<DeaconEvent>>) -> DeaconHandle {
    let (tx, mut rx) = mpsc::channel::<DeaconMsg>(64);
    tokio::spawn(async move {
        while let Some(_msg) = rx.recv().await {
            // TODO(hq-92z9 fill): dispatch messages against DeaconBoard + emit events.
        }
    });
    DeaconHandle { tx }
}
