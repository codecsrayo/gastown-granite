//! Issue-comment port (hq-fe-api-w.5). `POST /api/beads/:id/comments` is the
//! dashboard's "add a note" button — appends an operator-supplied fragment to
//! the `hq.issues.notes` column. The trait keeps `gt-web` decoupled from the
//! Dolt edge: production cables [`DoltIssueCommenter`] over a shared
//! [`gt_store_dolt::DoltIssues`] handle (the same one `GET /api/issues` reads),
//! tests use [`InMemoryIssueCommenter`] to assert the route formed the
//! canonical fragment and forwarded it to the edge.
//!
//! Storage shape: comments are flat text appended to the existing `notes`
//! column, not a separate `issue_comments` table. The migration plan tracks
//! the future structured shape; this route ships the minimum viable comments
//! surface so the kanban can capture context today.

use std::sync::{Arc, Mutex};

use gt_events::AppError;
use gt_store_dolt::DoltIssues;

/// Edge port: append a text fragment to an issue's `notes` column. Cheap to
/// clone (Arc-backed impls) so the gateway can hand it to handlers via
/// [`crate::AppState`].
pub trait IssueCommenter: Send + Sync {
    /// Append `fragment` to `issues.notes` for `id` (atomic SQL CONCAT in the
    /// production impl). Returns `AppError::NotFound` when no such issue.
    fn append(
        &self,
        id: &str,
        fragment: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), AppError>> + Send + '_>>;
}

/// Production adapter: forwards to `DoltIssues::append_notes`. The shared
/// `Arc<DoltIssues>` is the same handle `GET /api/issues` reads from, so the
/// append path can never desync from the canonical row.
pub struct DoltIssueCommenter {
    inner: Arc<DoltIssues>,
}

impl DoltIssueCommenter {
    pub fn new(inner: Arc<DoltIssues>) -> Self {
        Self { inner }
    }
}

impl IssueCommenter for DoltIssueCommenter {
    fn append(
        &self,
        id: &str,
        fragment: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), AppError>> + Send + '_>> {
        let inner = self.inner.clone();
        let id = id.to_string();
        let fragment = fragment.to_string();
        Box::pin(async move { inner.append_notes(&id, &fragment).await })
    }
}

/// Test double: records every (id, fragment) the handler issued so gates can
/// assert the route reached the edge with the canonical comment shape.
#[derive(Default, Clone)]
pub struct InMemoryIssueCommenter {
    appended: Arc<Mutex<Vec<(String, String)>>>,
    /// Set of ids the next `append` should reject with `NotFound`. Lets gates
    /// exercise the 404 path without a real Dolt connection.
    not_found: Arc<Mutex<Vec<String>>>,
}

impl InMemoryIssueCommenter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn appended(&self) -> Vec<(String, String)> {
        self.appended.lock().unwrap().clone()
    }

    /// Mark `id` as "missing" so the next `append(id, _)` returns 404.
    pub fn set_not_found(&self, id: impl Into<String>) {
        self.not_found.lock().unwrap().push(id.into());
    }
}

impl IssueCommenter for InMemoryIssueCommenter {
    fn append(
        &self,
        id: &str,
        fragment: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), AppError>> + Send + '_>> {
        let appended = self.appended.clone();
        let not_found = self.not_found.clone();
        let id = id.to_string();
        let fragment = fragment.to_string();
        Box::pin(async move {
            if not_found.lock().unwrap().iter().any(|x| x == &id) {
                return Err(AppError::NotFound(format!("issue {id}")));
            }
            appended.lock().unwrap().push((id, fragment));
            Ok(())
        })
    }
}
