//! Persistence port for Sheriff. Same shape as `MergeRepository` (returns
//! `impl Future + Send` so the actor can `.await` without `async_trait` / `dyn`).
//! **Scaffolding stub** — in-memory only; the Dolt adapter lands with behavior.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Mutex;

use gt_events::AppError;

use crate::state::SheriffItem;

pub trait SheriffRepository: Send + Sync {
    fn upsert_item(&self, item: &SheriffItem) -> impl Future<Output = Result<(), AppError>> + Send;
    fn get_item(&self, id: &str) -> impl Future<Output = Result<Option<SheriffItem>, AppError>> + Send;
}

#[derive(Default)]
pub struct InMemorySheriffRepo {
    inner: Mutex<BTreeMap<String, SheriffItem>>,
}

impl SheriffRepository for InMemorySheriffRepo {
    async fn upsert_item(&self, item: &SheriffItem) -> Result<(), AppError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| AppError::Other("gt-sheriff repo poisoned".into()))?;
        g.insert(item.id.clone(), item.clone());
        Ok(())
    }

    async fn get_item(&self, id: &str) -> Result<Option<SheriffItem>, AppError> {
        let g = self
            .inner
            .lock()
            .map_err(|_| AppError::Other("gt-sheriff repo poisoned".into()))?;
        Ok(g.get(id).cloned())
    }
}
