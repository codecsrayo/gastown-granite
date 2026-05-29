use serde::{Deserialize, Serialize};

use gt_events::EventKind;

/// Eventos del dominio scheduling. `Serialize`/`Deserialize` para el log + replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SchedEvent {
    Enqueue { bead: String, priority: u8 },
    Dispatched { bead: String, worker: String },
    DispatchFailed { bead: String, reason: String },
    DispatchTimeout { bead: String },
}

impl EventKind for SchedEvent {
    fn kind(&self) -> &'static str {
        match self {
            SchedEvent::Enqueue { .. } => "scheduling.enqueue",
            SchedEvent::Dispatched { .. } => "scheduling.dispatched",
            SchedEvent::DispatchFailed { .. } => "scheduling.dispatch_failed",
            SchedEvent::DispatchTimeout { .. } => "scheduling.dispatch_timeout",
        }
    }
}
