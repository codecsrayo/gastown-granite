//! hq-mysw (Paso 9.F) gate: the operator-signal path is wired end to end.
//!
//! A synthetic `*Stuck` gap — driven through the **Witness** (`witness.escalation_raised`),
//! the **Merge** lane (`merge.failed`), and the **Quota** domain (`quota.account_limited`) —
//! reaches the injected [`FakeNotifier`], and the stuck escalations leave a durable status
//! bead. This proves the three pieces of hq-mysw meet: feed/role gap detection → escalation
//! action (status bead + notify) → Notifier port (fake captures).
//!
//! The escalation status bead is created synchronously in the reactor (awaited repo write);
//! the notification is captured synchronously by the fake. So polling the repo + the fake is
//! the source of truth, exactly like the composition gate polls the audit log.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gt_beads::{Bead, BeadRepository, BeadStatus, InMemoryBeads};
use gt_notify::{FakeNotifier, Notification, Severity};
use gt_root::{spawn, Clock, Effects, RootConfig};

/// Edge clock — fixed value is fine: the witness uses producer-supplied `now_secs`, not this.
#[derive(Clone)]
struct FixedClock(u64);
impl Clock for FixedClock {
    fn now_secs(&self) -> u64 {
        self.0
    }
}

/// Records rotate/sling calls so the quota-block reaction's rotation is observable.
#[derive(Clone, Default)]
struct RecordingEffects {
    rotations: Arc<Mutex<Vec<String>>>,
}
impl Effects for RecordingEffects {
    fn sling(&self, _convoy: &str, _member: &str) {}
    fn rotate(&self, account: &str) {
        self.rotations.lock().unwrap().push(account.into());
    }
}

fn captured_with_tag(fake: &FakeNotifier, tag: &str) -> Vec<Notification> {
    fake.captured()
        .into_iter()
        .filter(|n| n.signal.tag() == tag)
        .collect()
}

/// Poll an async bead lookup until present or timeout.
async fn wait_for_bead(repo: &Arc<InMemoryBeads>, id: &str, timeout: Duration) -> Bead {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(b) = repo.get(id).await.unwrap() {
            return b;
        }
        if Instant::now() >= deadline {
            panic!("timeout waiting for bead {id}");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Poll the fake until at least one notification with `tag` is captured, or timeout.
async fn wait_for_note(fake: &FakeNotifier, tag: &str, timeout: Duration) -> Vec<Notification> {
    let deadline = Instant::now() + timeout;
    loop {
        let hits = captured_with_tag(fake, tag);
        if !hits.is_empty() {
            return hits;
        }
        if Instant::now() >= deadline {
            panic!("timeout waiting for {tag} notification");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn synthetic_stuck_raises_escalation_bead_and_notifies_via_notifier() {
    let repo = Arc::new(InMemoryBeads::default());
    let log_path = std::env::temp_dir().join(format!(
        "gt-mysw-{}-{}.events.jsonl",
        std::process::id(),
        ulid::Ulid::new()
    ));
    let _ = std::fs::remove_file(&log_path);

    let fake = FakeNotifier::new();
    let effects = RecordingEffects::default();
    let rotations = effects.rotations.clone();

    let root = spawn(
        repo.clone(),
        Arc::new(gt_merge::InMemoryMergeRepo::default()),
        Arc::new(gt_patrol::InMemoryPatrolRepo::default()),
        Arc::new(gt_orchestration::InMemoryOrchRepo::default()),
        effects,
        FixedClock(1_000),
        &log_path,
        RootConfig {
            notifier: Arc::new(fake.clone()),
            ..RootConfig::default()
        },
    );

    let timeout = Duration::from_secs(3);

    // --- 1) Witness path: a watched worker crosses its threshold → EscalationRaised --------
    // watch at since=100, threshold=30; tick at now=300 → age 200 >= 30 → escalation.
    gt_witness::witness::watch(&root.witness, "w1", 100, 30)
        .await
        .unwrap();
    gt_witness::witness::tick(&root.witness, "w1", 300)
        .await
        .unwrap();

    let bead = wait_for_bead(&repo, "escalation-w1", timeout).await;
    assert_eq!(bead.status, BeadStatus::Failed, "escalation bead is a problem state");
    assert_eq!(bead.priority, 0, "escalation is P0");
    assert!(bead.title.contains("w1"));

    let worker_notes = wait_for_note(&fake, "worker_stuck", timeout).await;
    assert_eq!(worker_notes.len(), 1);
    assert_eq!(worker_notes[0].severity, Severity::Urgent);

    // --- 2) Merge-stuck path: a failed merge slot escalates -------------------------------
    root.merge.submit("m1", "feat/x", "msg-01").await;
    root.merge.start("m1").await;
    root.merge.fail("m1", "merge conflict in foo.rs").await;

    let merge_bead = wait_for_bead(&repo, "escalation-merge-m1", timeout).await;
    assert_eq!(merge_bead.status, BeadStatus::Failed);
    assert!(merge_bead.title.contains("m1"));
    let merge_notes = wait_for_note(&fake, "merge_stuck", timeout).await;
    assert_eq!(merge_notes.len(), 1);

    // --- 3) Quota-block path: account limited → notification only, NO status bead ---------
    root.quota.probe("acc-1", 5_000, 6_000, 100).await;
    root.quota.limited("acc-1", 200).await;

    wait_for_note(&fake, "quota_block", timeout).await;

    // The corrective rotation still fired (quota-block is notify + rotate, no bead).
    let deadline = Instant::now() + timeout;
    while !rotations.lock().unwrap().iter().any(|a| a == "acc-1") {
        if Instant::now() >= deadline {
            panic!("timeout waiting for rotation of acc-1");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        repo.get("escalation-acc-1").await.unwrap().is_none(),
        "quota-block is notification-only — no status bead",
    );

    root.shutdown();
    let _ = std::fs::remove_file(&log_path);
}
