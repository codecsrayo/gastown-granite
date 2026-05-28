//! Frontier-audit events emitted by the `gt-web` HTTP boundary. Doc 07 puts auth/IAM at the
//! gateway, not in the domains, and requires "audit who-consulted-what" to land in the same
//! event log the domain events share. Records use the `web.*` prefix so domain replay
//! (`gt_root::replay_gt`) skips them — they carry no domain state — while the feed still
//! folds them for observability, mirroring the `mcp.*` frontier-audit pattern.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use gt_audit::{EventRecord, EventStore};
use gt_events::{Envelope, EventKind};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WebAuditEvent {
    /// The boundary accepted the request: bearer token validated and the handler ran.
    Invoked {
        actor: String,
        method: String,
        path: String,
        status: u16,
    },
    /// The boundary rejected the request before dispatching (missing/invalid token).
    Unauthorized {
        method: String,
        path: String,
        reason: String,
    },
}

impl EventKind for WebAuditEvent {
    fn kind(&self) -> &'static str {
        match self {
            WebAuditEvent::Invoked { .. } => "web.invoked",
            WebAuditEvent::Unauthorized { .. } => "web.unauthorized",
        }
    }
}

pub trait WebAuditSink: Send + Sync {
    fn record(&self, event: WebAuditEvent);
}

#[derive(Debug, Default, Clone)]
pub struct InMemoryWebAudit {
    events: Arc<Mutex<Vec<WebAuditEvent>>>,
}

impl InMemoryWebAudit {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> Vec<WebAuditEvent> {
        self.events.lock().unwrap().clone()
    }

    pub fn drain(&self) -> Vec<WebAuditEvent> {
        std::mem::take(&mut *self.events.lock().unwrap())
    }
}

impl WebAuditSink for InMemoryWebAudit {
    fn record(&self, event: WebAuditEvent) {
        self.events.lock().unwrap().push(event);
    }
}

impl<T: WebAuditSink + ?Sized> WebAuditSink for Arc<T> {
    fn record(&self, event: WebAuditEvent) {
        (**self).record(event)
    }
}

/// Persists each invocation to the gt-audit event log as a type-erased [`EventRecord`].
/// Mirrors `gt_mcp::JsonlAudit`: failures go to stderr; the append is best-effort frontier
/// auditing, not a domain mutation.
pub struct JsonlWebAudit {
    store: Arc<dyn EventStore + Send + Sync>,
}

impl JsonlWebAudit {
    pub fn new(store: Arc<dyn EventStore + Send + Sync>) -> Self {
        Self { store }
    }
}

impl WebAuditSink for JsonlWebAudit {
    fn record(&self, event: WebAuditEvent) {
        let env = Envelope::root(event);
        match EventRecord::from_envelope(&env) {
            Ok(rec) => {
                if let Err(e) = self.store.append(&rec) {
                    eprintln!("[gt-web] audit append failed: {e}");
                }
            }
            Err(e) => eprintln!("[gt-web] audit encode failed: {e}"),
        }
    }
}
