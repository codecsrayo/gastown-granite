//! Domain events of the Mayor role. **Scaffolding stub** — variants land with behavior.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum MayorEvent {
    /// Placeholder mantenida hasta el commit de behavior (hq-92z9).
    Placeholder,
}

impl gt_events::EventKind for MayorEvent {
    fn kind(&self) -> &'static str {
        match self {
            MayorEvent::Placeholder => "mayor.placeholder",
        }
    }
}
