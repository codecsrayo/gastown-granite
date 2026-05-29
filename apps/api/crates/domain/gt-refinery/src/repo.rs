//! Persistence port for Refinery. Same shape as `MergeRepository` (returns
//! `impl Future + Send` so the actor can `.await` without `async_trait` / `dyn`).
//! **Scaffolding stub** — in-memory only; the Dolt adapter lands with behavior.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Mutex;

use gt_events::AppError;

use crate::state::RefineryItem;

pub trait RefineryRepository: Send + Sync {
    fn upsert_item(&self, item: &RefineryItem) -> impl Future<Output = Result<(), AppError>> + Send;
    fn get_item(&self, id: &str) -> impl Future<Output = Result<Option<RefineryItem>, AppError>> + Send;
}

#[derive(Default)]
pub struct InMemoryRefineryRepo {
    inner: Mutex<BTreeMap<String, RefineryItem>>,
}

impl RefineryRepository for InMemoryRefineryRepo {
    async fn upsert_item(&self, item: &RefineryItem) -> Result<(), AppError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| AppError::Other("gt-refinery repo poisoned".into()))?;
        g.insert(item.id.clone(), item.clone());
        Ok(())
    }

    async fn get_item(&self, id: &str) -> Result<Option<RefineryItem>, AppError> {
        let g = self
            .inner
            .lock()
            .map_err(|_| AppError::Other("gt-refinery repo poisoned".into()))?;
        Ok(g.get(id).cloned())
    }
}
