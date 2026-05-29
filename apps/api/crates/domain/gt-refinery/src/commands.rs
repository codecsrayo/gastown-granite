//! Owned `Command` structs over `RefineryBoard`. **Scaffolding stub** — variants land
//! with the Refinery behavior commit. `validate` is the no-op identity until then.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use gt_events::AppError;

use crate::state::RefineryBoard;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum RefineryCommand {
    /// Placeholder mantenida hasta el commit de behavior (hq-92z9).
    Placeholder,
}

impl RefineryCommand {
    pub fn validate(&self, _board: &RefineryBoard) -> Result<(), AppError> {
        Ok(())
    }
}
