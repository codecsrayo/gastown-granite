//! Actor dueño del `SessionRegistry`. El estado vive en UNA task; el resto le habla por
//! `mpsc`. Sin `Arc<Mutex>`: los datos se mueven por canales, no se comparten por referencia.

use tokio::sync::{mpsc, oneshot};

use gt_events::AppError;

use crate::state::{Session, SessionRegistry, SessionState};

/// Mensajes al actor. `Snapshot`/`Transition` responden por un `oneshot`.
pub enum AgentMsg {
    Add(Session),
    Remove(String),
    Transition {
        id: String,
        to: SessionState,
        reply: oneshot::Sender<Result<(), AppError>>,
    },
    Snapshot(oneshot::Sender<Vec<Session>>),
}

/// Handle clonable para hablarle al actor.
#[derive(Clone)]
pub struct AgentHandle {
    tx: mpsc::Sender<AgentMsg>,
}

impl AgentHandle {
    pub async fn add(&self, session: Session) {
        let _ = self.tx.send(AgentMsg::Add(session)).await;
    }

    pub async fn remove(&self, id: impl Into<String>) {
        let _ = self.tx.send(AgentMsg::Remove(id.into())).await;
    }

    pub async fn transition(&self, id: impl Into<String>, to: SessionState) -> Result<(), AppError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(AgentMsg::Transition {
                id: id.into(),
                to,
                reply,
            })
            .await
            .map_err(|_| AppError::Other("actor gone".into()))?;
        rx.await.map_err(|_| AppError::Other("actor dropped reply".into()))?
    }

    pub async fn snapshot(&self) -> Vec<Session> {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(AgentMsg::Snapshot(reply)).await.is_err() {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }
}

/// Arranca el actor y devuelve su handle. El mailbox es bounded (contrapresión correcta).
pub fn spawn(buffer: usize) -> AgentHandle {
    let (tx, mut rx) = mpsc::channel::<AgentMsg>(buffer);
    tokio::spawn(async move {
        let mut reg = SessionRegistry::default();
        while let Some(msg) = rx.recv().await {
            match msg {
                AgentMsg::Add(s) => reg.add(s),
                AgentMsg::Remove(id) => {
                    reg.remove(&id);
                }
                AgentMsg::Transition { id, to, reply } => {
                    let _ = reply.send(reg.transition(&id, to));
                }
                AgentMsg::Snapshot(reply) => {
                    let _ = reply.send(reg.snapshot());
                }
            }
        }
    });
    AgentHandle { tx }
}
