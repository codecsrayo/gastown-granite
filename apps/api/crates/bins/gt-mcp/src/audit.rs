//! Every MCP invocation produces an audit record. The sink is a port so the binary
//! can write to the gt-audit log while tests can capture into memory.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use gt_audit::{EventRecord, EventStore};
use gt_events::{Envelope, EventKind};

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

/// Routing key on the wire. The `mcp.` prefix marks these as frontier-audit (observability)
/// events: they share the system event log but carry no domain state, so domain replay skips
/// the prefix (`gt-root::replay_gt`) while the feed still folds them.
impl EventKind for AuditEvent {
    fn kind(&self) -> &'static str {
        match self {
            AuditEvent::Invoked { .. } => "mcp.invoked",
            AuditEvent::Unauthorized { .. } => "mcp.unauthorized",
        }
    }
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

/// `AuditSink` that persists each invocation to the gt-audit event log as a type-erased
/// [`EventRecord`]. The event id, correlation id and timestamp are stamped here at the edge
/// via [`Envelope::root`] (the determinism rule keeps these out of the core — `docs/06`).
///
/// The sink trait returns `()`, so a write failure is logged to stderr rather than dropped
/// silently; the events.jsonl append is best-effort frontier auditing, not a domain mutation.
pub struct JsonlAudit {
    store: Arc<dyn EventStore + Send + Sync>,
}

impl JsonlAudit {
    pub fn new(store: Arc<dyn EventStore + Send + Sync>) -> Self {
        Self { store }
    }
}

impl AuditSink for JsonlAudit {
    fn record(&self, event: AuditEvent) {
        let env = Envelope::root(event);
        match EventRecord::from_envelope(&env) {
            Ok(rec) => {
                if let Err(e) = self.store.append(&rec) {
                    eprintln!("[gt-mcp] audit append failed: {e}");
                }
            }
            Err(e) => eprintln!("[gt-mcp] audit encode failed: {e}"),
        }
    }
}
