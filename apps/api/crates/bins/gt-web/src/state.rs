//! Shared state carried by every handler. Generic over the ports so the bin plugs in real
//! adapters and tests plug in in-memory ones; `Arc`s give cheap clones for axum's State.

use std::sync::Arc;

use tokio::sync::{broadcast, mpsc};

use gt_agent::{AgentEvent, SessionQueries};
use gt_audit::EventRecord;
use gt_beads::BeadRepository;
use gt_events::Envelope;

/// Read-side composition root. Keep it `Clone` (Arc-backed) so axum can hand a fresh handle
/// to each request without locks. `R`/`SQ` are the bead / session ports.
pub struct AppState<R, SQ>
where
    R: BeadRepository + Send + Sync + 'static,
    SQ: SessionQueries + Send + Sync + 'static,
{
    pub beads: Arc<R>,
    pub sessions: Arc<SQ>,
    pub agent_events: mpsc::Sender<Envelope<AgentEvent>>,
    pub events: broadcast::Sender<EventRecord>,
}

impl<R, SQ> Clone for AppState<R, SQ>
where
    R: BeadRepository + Send + Sync + 'static,
    SQ: SessionQueries + Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        Self {
            beads: self.beads.clone(),
            sessions: self.sessions.clone(),
            agent_events: self.agent_events.clone(),
            events: self.events.clone(),
        }
    }
}
