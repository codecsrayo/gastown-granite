use gt_events::AppError;

use crate::bead::{Bead, BeadStatus};

/// Puerto de persistencia de beads. Async porque modela almacenamiento; se usa por
/// genéricos. Lo implementan `InMemoryBeads` (tests) y `gt-store-dolt` (Dolt). El mismo
/// test de contrato debe pasar contra ambos (gate del Paso 4).
#[allow(async_fn_in_trait)]
pub trait BeadRepository {
    /// Inserta o reemplaza un bead por id.
    async fn upsert(&self, bead: &Bead) -> Result<(), AppError>;

    /// Obtiene un bead por id.
    async fn get(&self, id: &str) -> Result<Option<Bead>, AppError>;

    /// Lista beads en un estado dado (la "cola" es una vista sobre el estado).
    async fn list_by_status(&self, status: BeadStatus) -> Result<Vec<Bead>, AppError>;

    /// Claim atómico por **CAS**: pasa el bead de `pending` a `dispatched` asignándolo.
    /// Devuelve `true` si este llamador ganó (gano quien lo encontró aún `pending`),
    /// `false` si otro lo reclamó. Portable y alineado con el grano nativo de Dolt
    /// (ver `docs/05-queues.md`).
    async fn cas_claim(&self, id: &str, worker: &str) -> Result<bool, AppError>;
}
