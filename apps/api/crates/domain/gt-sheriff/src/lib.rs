//! `gt-sheriff` — watchdog Plugin (Paso 9.D, stubs gt-plugin until hq-evks).
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
pub mod sheriff;
mod events;
mod repo;
mod state;

pub use actor::{spawn, SheriffHandle, SheriffMsg};
pub use commands::SheriffCommand;
pub use events::SheriffEvent;
pub use repo::{InMemorySheriffRepo, SheriffRepository};
pub use state::{SheriffBoard, SheriffItem};
