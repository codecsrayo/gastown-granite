//! `mayor` — producer for the Mayor role. **Scaffolding stub (hq-92z9):**
//! the real producer body (orchestration loop (Paso 9.D)) lands in the per-role behavior commit.

use crate::actor::MayorHandle;

/// Spawn the Mayor producer. Scaffolding: returns immediately, no background task.
pub fn spawn(_handle: MayorHandle) {
    // TODO(hq-92z9 fill): subscribe to bus / channel and forward messages to the actor.
}
