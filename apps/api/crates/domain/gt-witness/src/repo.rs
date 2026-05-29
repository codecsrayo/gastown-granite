//! Persistence port for Witness. Same shape as `MergeRepository` (returns
//! `impl Future + Send` so the actor can `.await` without `async_trait` / `dyn`).
//! **Scaffolding stub** — in-memory only; the Dolt adapter lands with behavior.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Mutex;

use gt_events::AppError;

use crate::state::WitnessItem;

pub trait WitnessRepository: Send + Sync {
    fn upsert_item(&self, item: &WitnessItem) -> impl Future<Output = Result<(), AppError>> + Send;
    fn get_item(&self, id: &str) -> impl Future<Output = Result<Option<WitnessItem>, AppError>> + Send;
}

#[derive(Default)]
pub struct InMemoryWitnessRepo {
    inner: Mutex<BTreeMap<String, WitnessItem>>,
}

impl WitnessRepository for InMemoryWitnessRepo {
    async fn upsert_item(&self, item: &WitnessItem) -> Result<(), AppError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| AppError::Other("gt-witness repo poisoned".into()))?;
        g.insert(item.id.clone(), item.clone());
        Ok(())
    }

    async fn get_item(&self, id: &str) -> Result<Option<WitnessItem>, AppError> {
        let g = self
            .inner
            .lock()
            .map_err(|_| AppError::Other("gt-witness repo poisoned".into()))?;
        Ok(g.get(id).cloned())
    }
}
