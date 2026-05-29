//! State of the Mayor domain. **Scaffolding stub** — real state machine + transitions
//! land with the Mayor behavior commit.

use std::collections::BTreeMap;

use gt_events::AppError;

use crate::events::MayorEvent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MayorItem {
    pub id: String,
    // TODO(hq-92z9 fill): per-role fields + state machine.
}

#[derive(Debug, Default)]
pub struct MayorBoard {
    pub items: BTreeMap<String, MayorItem>,
}

impl MayorBoard {
    /// Apply an event to the board. Scaffolding stub: accepts any event, mutates nothing.
    pub fn apply(&mut self, _ev: &MayorEvent) -> Result<(), AppError> {
        Ok(())
    }
}
