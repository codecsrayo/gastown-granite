//! Persistence port for Deacon. Same shape as `MergeRepository` (returns
//! `impl Future + Send` so the actor can `.await` without `async_trait` / `dyn`).
//! **Scaffolding stub** — in-memory only; the Dolt adapter lands with behavior.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Mutex;

use gt_events::AppError;

use crate::state::DeaconItem;

pub trait DeaconRepository: Send + Sync {
    fn upsert_item(&self, item: &DeaconItem) -> impl Future<Output = Result<(), AppError>> + Send;
    fn get_item(&self, id: &str) -> impl Future<Output = Result<Option<DeaconItem>, AppError>> + Send;
}

#[derive(Default)]
pub struct InMemoryDeaconRepo {
    inner: Mutex<BTreeMap<String, DeaconItem>>,
}

impl DeaconRepository for InMemoryDeaconRepo {
    async fn upsert_item(&self, item: &DeaconItem) -> Result<(), AppError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| AppError::Other("gt-deacon repo poisoned".into()))?;
        g.insert(item.id.clone(), item.clone());
        Ok(())
    }

    async fn get_item(&self, id: &str) -> Result<Option<DeaconItem>, AppError> {
        let g = self
            .inner
            .lock()
            .map_err(|_| AppError::Other("gt-deacon repo poisoned".into()))?;
        Ok(g.get(id).cloned())
    }
}
