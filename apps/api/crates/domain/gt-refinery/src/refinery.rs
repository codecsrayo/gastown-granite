//! `refinery` — producer for the Refinery role. **Scaffolding stub (hq-92z9):**
//! the real producer body (merge gates await MERGE_READY (Paso 9.D)) lands in the per-role behavior commit.

use crate::actor::RefineryHandle;

/// Spawn the Refinery producer. Scaffolding: returns immediately, no background task.
pub fn spawn(_handle: RefineryHandle) {
    // TODO(hq-92z9 fill): subscribe to bus / channel and forward messages to the actor.
}
