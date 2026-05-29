//! Owned `Command` structs over `WitnessBoard`. **Scaffolding stub** — variants land
//! with the Witness behavior commit. `validate` is the no-op identity until then.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use gt_events::AppError;

use crate::state::WitnessBoard;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum WitnessCommand {
    /// Placeholder mantenida hasta el commit de behavior (hq-92z9).
    Placeholder,
}

impl WitnessCommand {
    pub fn validate(&self, _board: &WitnessBoard) -> Result<(), AppError> {
        Ok(())
    }
}
