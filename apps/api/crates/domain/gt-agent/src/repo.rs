use std::future::Future;
use std::sync::Mutex;

use gt_events::AppError;

use crate::state::Session;

/// Puerto de consulta del dominio (lado lectura). Async porque modela almacenamiento
/// (en Paso 4 lo implementa `gt-store-dolt`); aquí basta una implementación in-memory.
/// Se usa por genéricos, no por `dyn` (ver `docs/01-architecture.md`). Devuelve
/// `impl Future + Send` (RPITIT) — alineado con `gt-beads::BeadRepository` — para que el
/// puerto sea utilizable desde tasks de tokio (incluyendo axum, que exige `Send`).
pub trait SessionQueries: Send + Sync {
    fn active_sessions(&self) -> impl Future<Output = Result<Vec<Session>, AppError>> + Send;
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
    fn active_sessions(&self) -> impl Future<Output = Result<Vec<Session>, AppError>> + Send {
        let rows: Vec<Session> = {
            let all = self.sessions.lock().unwrap();
            all.iter().filter(|s| !s.is_terminal()).cloned().collect()
        };
        async move { Ok(rows) }
    }
}
