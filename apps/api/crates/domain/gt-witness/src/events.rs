//! Domain events of the Witness role. **Scaffolding stub** — variants land with behavior.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum WitnessEvent {
    /// Placeholder mantenida hasta el commit de behavior (hq-92z9).
    Placeholder,
}

impl gt_events::EventKind for WitnessEvent {
    fn kind(&self) -> &'static str {
        match self {
            WitnessEvent::Placeholder => "witness.placeholder",
        }
    }
}
