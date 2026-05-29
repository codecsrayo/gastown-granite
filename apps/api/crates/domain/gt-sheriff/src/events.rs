//! Domain events of the Sheriff role. **Scaffolding stub** — variants land with behavior.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum SheriffEvent {
    /// Placeholder mantenida hasta el commit de behavior (hq-92z9).
    Placeholder,
}

impl gt_events::EventKind for SheriffEvent {
    fn kind(&self) -> &'static str {
        match self {
            SheriffEvent::Placeholder => "sheriff.placeholder",
        }
    }
}
