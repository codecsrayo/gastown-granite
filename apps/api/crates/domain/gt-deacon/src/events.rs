//! Domain events of the Deacon role. **Scaffolding stub** — variants land with behavior.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum DeaconEvent {
    /// Placeholder mantenida hasta el commit de behavior (hq-92z9).
    Placeholder,
}

impl gt_events::EventKind for DeaconEvent {
    fn kind(&self) -> &'static str {
        match self {
            DeaconEvent::Placeholder => "deacon.placeholder",
        }
    }
}
