//! Owned `Command` structs over `DeaconBoard`. **Scaffolding stub** — variants land
//! with the Deacon behavior commit. `validate` is the no-op identity until then.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use gt_events::AppError;

use crate::state::DeaconBoard;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum DeaconCommand {
    /// Placeholder mantenida hasta el commit de behavior (hq-92z9).
    Placeholder,
}

impl DeaconCommand {
    pub fn validate(&self, _board: &DeaconBoard) -> Result<(), AppError> {
        Ok(())
    }
}
