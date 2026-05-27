use gt_events::EventKind;

/// Eventos del dominio agente. Enum **owned** (sin lifetimes, trivialmente `Send`); el
/// `match` de `kind()` es exhaustivo y lo verifica el compilador (añade variante y olvida
/// el brazo → no compila).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEvent {
    Spawned { session: String, rig: String },
    Heartbeat { session: String },
    SessionEnd { session: String },
    Killed { session: String, reason: String },
}

impl EventKind for AgentEvent {
    fn kind(&self) -> &'static str {
        match self {
            AgentEvent::Spawned { .. } => "agent.spawned",
            AgentEvent::Heartbeat { .. } => "agent.heartbeat",
            AgentEvent::SessionEnd { .. } => "agent.session_end",
            AgentEvent::Killed { .. } => "agent.killed",
        }
    }
}
