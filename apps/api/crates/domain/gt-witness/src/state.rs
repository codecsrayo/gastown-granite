//! State of the Witness domain. **Scaffolding stub** — real state machine + transitions
//! land with the Witness behavior commit.

use std::collections::BTreeMap;

use gt_events::AppError;

use crate::events::WitnessEvent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitnessItem {
    pub id: String,
    // TODO(hq-92z9 fill): per-role fields + state machine.
}

#[derive(Debug, Default)]
pub struct WitnessBoard {
    pub items: BTreeMap<String, WitnessItem>,
}

impl WitnessBoard {
    /// Apply an event to the board. Scaffolding stub: accepts any event, mutates nothing.
    pub fn apply(&mut self, _ev: &WitnessEvent) -> Result<(), AppError> {
        Ok(())
    }
}
