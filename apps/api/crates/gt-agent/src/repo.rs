use std::sync::Mutex;

use gt_events::AppError;

use crate::state::Session;

/// Puerto de consulta del dominio (lado lectura). Async porque modela almacenamiento
/// (en Paso 4 lo implementa `gt-store-dolt`); aquí basta una implementación in-memory.
/// Se usa por genéricos, no por `dyn` (ver `docs/01-architecture.md`).
#[allow(async_fn_in_trait)]
pub trait SessionQueries {
    async fn active_sessions(&self) -> Result<Vec<Session>, AppError>;
}

/// Implementación in-memory (sin Dolt). El mismo test del slice debe pasar luego contra el
/// adaptador real: si uno falla y el otro no, el puerto está mal definido (gate del Paso 4).
#[derive(Default)]
pub struct InMemorySessions {
    sessions: Mutex<Vec<Session>>,
}

impl InMemorySessions {
    pub fn new(sessions: Vec<Session>) -> Self {
        Self {
            sessions: Mutex::new(sessions),
        }
    }
}

impl SessionQueries for InMemorySessions {
    async fn active_sessions(&self) -> Result<Vec<Session>, AppError> {
        let all = self.sessions.lock().unwrap();
        Ok(all.iter().filter(|s| !s.is_terminal()).cloned().collect())
    }
}
