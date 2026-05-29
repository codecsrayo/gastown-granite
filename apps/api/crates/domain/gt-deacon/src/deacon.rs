//! `deacon` — producer for the Deacon role. **Scaffolding stub (hq-92z9):**
//! the real producer body (town shutdown / drain coordination (Paso 9.D)) lands in the per-role behavior commit.

use crate::actor::DeaconHandle;

/// Spawn the Deacon producer. Scaffolding: returns immediately, no background task.
pub fn spawn(_handle: DeaconHandle) {
    // TODO(hq-92z9 fill): subscribe to bus / channel and forward messages to the actor.
}
