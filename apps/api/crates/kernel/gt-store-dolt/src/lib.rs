//! `gt-store-dolt` — adaptador de BD: implementa los puertos de los dominios contra Dolt
//! por el cable MySQL (puerto 3307). Dependencia **invertida**: el dominio define el puerto
//! (`gt-beads::BeadRepository`), la infraestructura lo implementa aquí.
//!
//! Dolt es MySQL-compatible pero **optimista** (sin `SELECT ... FOR UPDATE`): el claim de la
//! cola se hace por **CAS** (`UPDATE ... WHERE status='pending'`), portable y alineado con
//! su grano nativo (ver `docs/05-queues.md`).

mod beads_repo;
mod commit;
mod conn;

pub use beads_repo::DoltBeads;
pub use commit::{commit, diff_summary, rollback};
pub use conn::connect;
