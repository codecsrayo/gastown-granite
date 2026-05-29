//! Persistence port for Mayor. Same shape as `MergeRepository` (returns
//! `impl Future + Send` so the actor can `.await` without `async_trait` / `dyn`).
//! **Scaffolding stub** — in-memory only; the Dolt adapter lands with behavior.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Mutex;

use gt_events::AppError;

use crate::state::MayorItem;

pub trait MayorRepository: Send + Sync {
    fn upsert_item(&self, item: &MayorItem) -> impl Future<Output = Result<(), AppError>> + Send;
    fn get_item(&self, id: &str) -> impl Future<Output = Result<Option<MayorItem>, AppError>> + Send;
}

#[derive(Default)]
pub struct InMemoryMayorRepo {
    inner: Mutex<BTreeMap<String, MayorItem>>,
}

impl MayorRepository for InMemoryMayorRepo {
    async fn upsert_item(&self, item: &MayorItem) -> Result<(), AppError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| AppError::Other("gt-mayor repo poisoned".into()))?;
        g.insert(item.id.clone(), item.clone());
        Ok(())
    }

    async fn get_item(&self, id: &str) -> Result<Option<MayorItem>, AppError> {
        let g = self
            .inner
            .lock()
            .map_err(|_| AppError::Other("gt-mayor repo poisoned".into()))?;
        Ok(g.get(id).cloned())
    }
}
