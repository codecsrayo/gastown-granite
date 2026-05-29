//! Owned `Command` structs over `SheriffBoard`. **Scaffolding stub** — variants land
//! with the Sheriff behavior commit. `validate` is the no-op identity until then.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use gt_events::AppError;

use crate::state::SheriffBoard;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum SheriffCommand {
    /// Placeholder mantenida hasta el commit de behavior (hq-92z9).
    Placeholder,
}

impl SheriffCommand {
    pub fn validate(&self, _board: &SheriffBoard) -> Result<(), AppError> {
        Ok(())
    }
}
