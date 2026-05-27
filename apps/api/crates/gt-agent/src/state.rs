use std::collections::HashMap;

use gt_events::AppError;

/// Ciclo de vida de una sesión. Las transiciones ilegales se rechazan (error semántico
/// atrapado por el tipo, ver `docs/06-observability.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Spawned,
    Working,
    Done,
    Killed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    pub rig: String,
    pub state: SessionState,
}

impl Session {
    pub fn new(id: impl Into<String>, rig: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            rig: rig.into(),
            state: SessionState::Spawned,
        }
    }

    /// Aplica una transición o la rechaza. Reglas (corrige el ejemplo incompleto del doc):
    /// `Spawned→Working→Done`, y `Killed` admisible desde cualquier estado **activo**.
    /// `Done`/`Killed` son terminales.
    pub fn transition(&mut self, to: SessionState) -> Result<(), AppError> {
        use SessionState::*;
        let legal = matches!(
            (self.state, to),
            (Spawned, Working) | (Working, Done) | (Spawned, Killed) | (Working, Killed)
        );
        if legal {
            self.state = to;
            Ok(())
        } else {
            Err(AppError::InvalidTransition(format!(
                "{:?} -> {:?}",
                self.state, to
            )))
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self.state, SessionState::Done | SessionState::Killed)
    }
}

/// Registro de sesiones — struct **owned**. Vive dentro de UNA task (el actor); nadie más
/// lo toca, así que no hay `Arc<Mutex>`. Derivación de estado pura → replay-able.
#[derive(Debug, Default)]
pub struct SessionRegistry {
    sessions: HashMap<String, Session>,
}

impl SessionRegistry {
    pub fn add(&mut self, session: Session) {
        self.sessions.insert(session.id.clone(), session);
    }

    pub fn remove(&mut self, id: &str) -> Option<Session> {
        self.sessions.remove(id)
    }

    pub fn get(&self, id: &str) -> Option<&Session> {
        self.sessions.get(id)
    }

    /// Aplica una transición a una sesión existente.
    pub fn transition(&mut self, id: &str, to: SessionState) -> Result<(), AppError> {
        match self.sessions.get_mut(id) {
            Some(s) => s.transition(to),
            None => Err(AppError::NotFound(format!("session {id}"))),
        }
    }

    /// Foto de todas las sesiones (orden no garantizado).
    pub fn snapshot(&self) -> Vec<Session> {
        self.sessions.values().cloned().collect()
    }

    /// Solo sesiones no-terminales.
    pub fn active(&self) -> Vec<Session> {
        self.sessions
            .values()
            .filter(|s| !s.is_terminal())
            .cloned()
            .collect()
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legal_lifecycle() {
        let mut s = Session::new("p1", "granite");
        assert_eq!(s.state, SessionState::Spawned);
        s.transition(SessionState::Working).unwrap();
        s.transition(SessionState::Done).unwrap();
        assert!(s.is_terminal());
    }

    #[test]
    fn illegal_skip_is_rejected() {
        let mut s = Session::new("p1", "granite");
        // Spawned -> Done sin pasar por Working: error semántico atrapado.
        assert!(s.transition(SessionState::Done).is_err());
        assert_eq!(s.state, SessionState::Spawned, "estado intacto tras rechazo");
    }

    #[test]
    fn kill_allowed_from_active_states() {
        let mut s = Session::new("p1", "granite");
        s.transition(SessionState::Killed).unwrap(); // desde Spawned
        assert!(s.transition(SessionState::Working).is_err()); // terminal, ya no se mueve
    }

    #[test]
    fn registry_active_excludes_terminal() {
        let mut reg = SessionRegistry::default();
        reg.add(Session::new("a", "r"));
        reg.add(Session::new("b", "r"));
        reg.transition("b", SessionState::Killed).unwrap();
        assert_eq!(reg.active().len(), 1);
        assert_eq!(reg.len(), 2);
    }
}
