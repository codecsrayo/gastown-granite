//! Domain events of the Refinery role. **Scaffolding stub** — variants land with behavior.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum RefineryEvent {
    /// Placeholder mantenida hasta el commit de behavior (hq-92z9).
    Placeholder,
}

impl gt_events::EventKind for RefineryEvent {
    fn kind(&self) -> &'static str {
        match self {
            RefineryEvent::Placeholder => "refinery.placeholder",
        }
    }
}
