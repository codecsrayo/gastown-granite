use std::future::Future;

use gt_events::AppError;

use crate::bead::{Bead, BeadStatus};

/// Puerto de persistencia de beads. Métodos devuelven `impl Future + Send` (RPITIT) para
/// que el puerto sea utilizable desde tasks de tokio sin `async_trait` ni boxing: el
/// `+ Send` explícito en el trait permite a `tokio::spawn` mover el futuro entre hilos.
///
/// Se usa por **genéricos**, no por `dyn`. Lo implementan `InMemoryBeads` (tests) y
/// `gt-store-dolt` (Dolt); el mismo test de contrato debe pasar contra ambos (gate Paso 4).
pub trait BeadRepository: Send + Sync {
    fn upsert(&self, bead: &Bead) -> impl Future<Output = Result<(), AppError>> + Send;

    fn get(&self, id: &str) -> impl Future<Output = Result<Option<Bead>, AppError>> + Send;

    fn list_by_status(
        &self,
        status: BeadStatus,
    ) -> impl Future<Output = Result<Vec<Bead>, AppError>> + Send;

    /// Claim atómico por **CAS**: pasa el bead de `pending` a `dispatched` asignándolo.
    /// `true` si este llamador ganó (encontró el bead `pending`), `false` si otro lo
    /// reclamó. Portable y alineado con Dolt (sin `SELECT FOR UPDATE`; ver `docs/05`).
    fn cas_claim(
        &self,
        id: &str,
        worker: &str,
    ) -> impl Future<Output = Result<bool, AppError>> + Send;
}

/// Delega a través de `Arc`, para compartir un repo entre el actor dueño y los lectores
/// (p. ej. el composition root) sin `Arc<Mutex>` repartido.
impl<R: BeadRepository + ?Sized> BeadRepository for std::sync::Arc<R> {
    fn upsert(&self, bead: &Bead) -> impl Future<Output = Result<(), AppError>> + Send {
        async move { (**self).upsert(bead).await }
    }
    fn get(&self, id: &str) -> impl Future<Output = Result<Option<Bead>, AppError>> + Send {
        async move { (**self).get(id).await }
    }
    fn list_by_status(
        &self,
        status: BeadStatus,
    ) -> impl Future<Output = Result<Vec<Bead>, AppError>> + Send {
        async move { (**self).list_by_status(status).await }
    }
    fn cas_claim(
        &self,
        id: &str,
        worker: &str,
    ) -> impl Future<Output = Result<bool, AppError>> + Send {
        async move { (**self).cas_claim(id, worker).await }
    }
}
