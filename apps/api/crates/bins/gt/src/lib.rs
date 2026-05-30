//! `gt` — the composition root of gastown-rs (Paso 6.e of `docs/08-getting-started.md`).
//!
//! This is the first crate in the `bins/` layer (`docs/01-architecture.md`): the one place
//! allowed to depend on every domain and wire them together. It contributes no domain logic
//! of its own — it only:
//!
//! 1. unifies the per-domain event enums into [`GtEvent`] / [`GtState`] (the "unificador"),
//! 2. spawns every domain actor and hands each a relay,
//! 3. drains all relays in a single async loop, appending every event to one audit log,
//! 4. fires the cross-domain reactions earlier steps deferred to "the composition root",
//! 5. captures reaction failures + undecodable events in a dead-letter.
//!
//! The pure unifier (`event`) is sync and replay-able; the wiring (`root`) is the async edge
//! with the side-effect and clock ports injected.

pub mod commands;
mod effects_real;
mod event;
mod notify_real;
pub mod root;

pub use commands::{CommandBus, RootCommand};
pub use effects_real::{QuotaSlot, RealEffects};
pub use event::{replay_gt, GtEvent, GtState};
pub use notify_real::MailNotifier;
pub use root::{
    load_state, spawn, spawn_hydrated, spawn_plugin_relay, spawn_sessions_projector, Clock,
    Effects, HydrationState, LogEffects, LogNotifier, RootConfig, RootHandle, SystemClock,
};
