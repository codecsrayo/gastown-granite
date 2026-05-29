//! `sheriff` — producer for the Sheriff role. **Scaffolding stub (hq-92z9):**
//! the real producer body (watchdog Plugin (Paso 9.D, stubs gt-plugin until hq-evks)) lands in the per-role behavior commit.

use crate::actor::SheriffHandle;

/// Spawn the Sheriff producer. Scaffolding: returns immediately, no background task.
pub fn spawn(_handle: SheriffHandle) {
    // TODO(hq-92z9 fill): subscribe to bus / channel and forward messages to the actor.
}
