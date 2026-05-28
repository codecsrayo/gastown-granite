//! Every MCP invocation produces an audit record. The sink is a port so the binary
//! can write to the gt-audit log while tests can capture into memory.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuditEvent {
    /// The frontier accepted the request and called validate/execute.
    Invoked {
        actor: String,
        tool: String,
        arguments: serde_json::Value,
        outcome: Outcome,
    },
    /// The scope rejected the request before dispatching.
    Unauthorized {
        actor: String,
        tool: String,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    Ok,
    Failed { error: String },
}

pub trait AuditSink: Send + Sync {
    fn record(&self, event: AuditEvent);
}

#[derive(Debug, Default, Clone)]
pub struct InMemoryAudit {
    events: Arc<Mutex<Vec<AuditEvent>>>,
}

impl InMemoryAudit {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn drain(&self) -> Vec<AuditEvent> {
        std::mem::take(&mut self.events.lock().unwrap())
    }

    pub fn snapshot(&self) -> Vec<AuditEvent> {
        self.events.lock().unwrap().clone()
    }
}

impl AuditSink for InMemoryAudit {
    fn record(&self, event: AuditEvent) {
        self.events.lock().unwrap().push(event);
    }
}

impl<T: AuditSink + ?Sized> AuditSink for Arc<T> {
    fn record(&self, event: AuditEvent) {
        (**self).record(event)
    }
}
