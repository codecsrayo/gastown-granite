//! Production [`Notifier`] adapter (hq-mysw): bead-backed mail.
//!
//! In Gas Town, mail *is* beads — the Go `internal/mail` package translates every message
//! to/from a beads issue. This is the Rust port of that delivery: each [`Notification`] becomes
//! a mail bead written through the [`BeadRepository`]. No SMTP/webhook dependency is pulled in;
//! the durable bead is the message, and the existing `gt mail` read-side surfaces it.
//!
//! Like [`crate::RealEffects`], the port method is sync and the actual persistence is a
//! fire-and-forget `tokio::spawn`: the reactor calls `notify` from the single-writer select
//! loop and must not block on a repo round-trip. A write failure is logged — the escalation's
//! own status bead (created synchronously in the reactor's `escalate`) is the durable record,
//! so a dropped mail does not lose the signal.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use gt_beads::{Bead, BeadRepository, BeadStatus};
use gt_notify::{Notification, Notifier};

/// Bead-backed mail sender. Generic over the repo (the port is RPITIT, not `dyn`); the bin
/// passes the same `Arc`-wrapped repo it gave the root, so mail beads land in the same store.
pub struct MailNotifier<R> {
    repo: R,
    /// `From` address stamped on the mail bead title (e.g. `"mayor/"`).
    from: String,
    /// `To` address — the operator mailbox that should receive escalations.
    to: String,
    /// Process-boot nanos: makes mail bead ids unique across restarts without an extra dep.
    boot_nanos: u128,
    /// Monotonic per-process sequence: makes ids unique within a run.
    seq: Arc<AtomicU64>,
}

impl<R> MailNotifier<R>
where
    R: BeadRepository + Clone + Send + Sync + 'static,
{
    pub fn new(repo: R, from: impl Into<String>, to: impl Into<String>) -> Self {
        Self {
            repo,
            from: from.into(),
            to: to.into(),
            boot_nanos: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            seq: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Render a notification as a mail bead. `Pending` is the inbox/unread state; mail beads are
    /// never `enqueue`d, so the scheduler (which only dispatches enqueued beads) never claims
    /// one. The id is unique per process+restart so repeated mails append instead of overwrite.
    fn mail_bead(&self, n: &Notification) -> Bead {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let id = format!("mail-{}-{}", self.boot_nanos, seq);
        let title = format!("[{}->{}] {}", self.from, self.to, n.subject);
        Bead::new(id, title, BeadStatus::Pending, mail_priority(n))
    }
}

/// Map notification severity onto bead priority (0 = P0, highest). Urgent escalations should
/// outrank routine notifications in any operator panel sorted by priority.
fn mail_priority(n: &Notification) -> u8 {
    use gt_notify::Severity::*;
    match n.severity {
        Urgent => 0,
        Warning => 1,
        Info => 2,
    }
}

impl<R> Notifier for MailNotifier<R>
where
    R: BeadRepository + Clone + Send + Sync + 'static,
{
    fn notify(&self, n: &Notification) {
        let repo = self.repo.clone();
        let bead = self.mail_bead(n);
        tokio::spawn(async move {
            if let Err(e) = repo.upsert(&bead).await {
                eprintln!("[gt] mail notify: upsert bead {} failed: {e}", bead.id);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gt_notify::{Notification, Signal};

    #[test]
    fn mail_bead_ids_are_unique_and_priority_tracks_severity() {
        // A throwaway repo: we only exercise `mail_bead`, not the spawn.
        let n = MailNotifier::new(Arc::new(gt_beads::InMemoryBeads::default()), "mayor/", "ops/");
        let urgent = n.mail_bead(&Notification::for_signal(Signal::WorkerStuck {
            worker: "w1".into(),
            age_secs: 900,
        }));
        let warn = n.mail_bead(&Notification::for_signal(Signal::QuotaBlock {
            account: "acc-1".into(),
        }));
        assert_ne!(urgent.id, warn.id, "each mail bead gets a distinct id");
        assert_eq!(urgent.priority, 0, "urgent → P0");
        assert_eq!(warn.priority, 1, "warning → P1");
        assert!(urgent.title.contains("mayor/"));
        assert_eq!(urgent.status, BeadStatus::Pending);
    }
}
