//! State of the Deacon domain. **Scaffolding stub** — real state machine + transitions
//! land with the Deacon behavior commit.

use std::collections::BTreeMap;

use gt_events::AppError;

use crate::events::DeaconEvent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeaconItem {
    pub id: String,
    // TODO(hq-92z9 fill): per-role fields + state machine.
}

#[derive(Debug, Default)]
pub struct DeaconBoard {
    pub items: BTreeMap<String, DeaconItem>,
}

impl DeaconBoard {
    /// Apply an event to the board. Scaffolding stub: accepts any event, mutates nothing.
    pub fn apply(&mut self, _ev: &DeaconEvent) -> Result<(), AppError> {
        Ok(())
    }
}
