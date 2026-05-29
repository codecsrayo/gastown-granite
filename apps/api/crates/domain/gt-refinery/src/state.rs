//! State of the Refinery domain. **Scaffolding stub** — real state machine + transitions
//! land with the Refinery behavior commit.

use std::collections::BTreeMap;

use gt_events::AppError;

use crate::events::RefineryEvent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefineryItem {
    pub id: String,
    // TODO(hq-92z9 fill): per-role fields + state machine.
}

#[derive(Debug, Default)]
pub struct RefineryBoard {
    pub items: BTreeMap<String, RefineryItem>,
}

impl RefineryBoard {
    /// Apply an event to the board. Scaffolding stub: accepts any event, mutates nothing.
    pub fn apply(&mut self, _ev: &RefineryEvent) -> Result<(), AppError> {
        Ok(())
    }
}
