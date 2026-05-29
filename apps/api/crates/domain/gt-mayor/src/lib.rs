//! `gt-mayor` — orchestration loop (Paso 9.D).
//!
//! **Scaffolding (hq-92z9 paso 1):** estructura de archivos según patrón `gt-merge`
//! (actor + commands + events + state + repo + producer). Las variantes reales de
//! commands/events y el loop del actor/productor se rellenan en commits subsiguientes
//! del mismo bead. Mantener este crate compilable es la única invariante de este pase.
//!
//! Aislamiento: depende solo del kernel (`gt-events`, `gt-channel`). La integración
//! cross-dominio se cablea en el composition root vía eventos.

pub mod actor;
pub mod commands;
pub mod mayor;
mod events;
mod repo;
mod state;

pub use actor::{spawn, MayorHandle, MayorMsg};
pub use commands::MayorCommand;
pub use events::MayorEvent;
pub use repo::{InMemoryMayorRepo, MayorRepository};
pub use state::{MayorBoard, MayorItem};
