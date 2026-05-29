//! State of the Sheriff domain. **Scaffolding stub** — real state machine + transitions
//! land with the Sheriff behavior commit.

use std::collections::BTreeMap;

use gt_events::AppError;

use crate::events::SheriffEvent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SheriffItem {
    pub id: String,
    // TODO(hq-92z9 fill): per-role fields + state machine.
}

#[derive(Debug, Default)]
pub struct SheriffBoard {
    pub items: BTreeMap<String, SheriffItem>,
}

impl SheriffBoard {
    /// Apply an event to the board. Scaffolding stub: accepts any event, mutates nothing.
    pub fn apply(&mut self, _ev: &SheriffEvent) -> Result<(), AppError> {
        Ok(())
    }
}
