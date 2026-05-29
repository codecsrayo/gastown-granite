//! `witness` — producer for the Witness role. **Scaffolding stub (hq-92z9):**
//! the real producer body (patrol vivo + escalation (Paso 9.D)) lands in the per-role behavior commit.

use crate::actor::WitnessHandle;

/// Spawn the Witness producer. Scaffolding: returns immediately, no background task.
pub fn spawn(_handle: WitnessHandle) {
    // TODO(hq-92z9 fill): subscribe to bus / channel and forward messages to the actor.
}
