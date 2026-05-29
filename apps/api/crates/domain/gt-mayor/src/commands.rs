//! Owned `Command` structs over `MayorBoard`. **Scaffolding stub** — variants land
//! with the Mayor behavior commit. `validate` is the no-op identity until then.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use gt_events::AppError;

use crate::state::MayorBoard;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum MayorCommand {
    /// Placeholder mantenida hasta el commit de behavior (hq-92z9).
    Placeholder,
}

impl MayorCommand {
    pub fn validate(&self, _board: &MayorBoard) -> Result<(), AppError> {
        Ok(())
    }
}
