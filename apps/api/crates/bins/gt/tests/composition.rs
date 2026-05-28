//! Paso 6.e gate (docs/08-getting-started.md): the composition root really binds it all.
//!
//! Drives a multi-domain scenario through ONE running root and ONE audit log:
//!
//! - **scheduling + patrol + agent**: enqueue → dispatch → register lease → stale lease →
//!   patrol tick → `LeaseExpired` → reactor reclaims the bead (`cas_release`) and re-enqueues
//!   → second dispatch to a fresh worker → completion. Same shape as Paso 6.a but the
//!   reactor is doing the work the test used to do inline.
//! - **orchestration**: a one-member convoy is launched, the `MemberDispatched` reaches the
//!   injected `Effects::sling` (the would-be `gt sling`), `member_done` closes the convoy.
//! - **agent**: a couple of edge-emitted `AgentEvent`s pushed through the agent relay land
//!   in the same log and rebuild the same `SessionRegistry` under replay.
//!
//! Then the gate fact: the single log replays back to the same per-domain states two ways
//! at once — independent per-domain prefix replay (the Paso 3 determinism guarantee, kept
//! intact) AND the unified `replay_gt`/`GtState` rebuild byte-identically.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gt_audit::{read_all, replay, EventRecord};
use gt_beads::{Bead, BeadRepository, BeadStatus, InMemoryBeads};
use gt_events::Envelope;

use gt_agent::{AgentEvent, SessionRegistry};
use gt_merge::{MergeEvent, MergeState};
use gt_orchestration::{OrchEvent, OrchState};
use gt_patrol::{PatrolEvent, PatrolState};
use gt_quota::{QuotaEvent, QuotaState};
use gt_scheduling::{SchedEvent, SchedState};

use gt_root::{replay_gt, spawn, Clock, Effects, RootConfig};

const LEASE_TIMEOUT: u64 = 30;

/// Test clock the gate advances by hand. Keeps the lease-expiry deterministic without
/// having to sleep real wall time.
#[derive(Clone)]
struct ManualClock(Arc<AtomicU64>);

impl ManualClock {
    fn new(start: u64) -> Self {
        Self(Arc::new(AtomicU64::new(start)))
    }
    fn set(&self, secs: u64) {
        self.0.store(secs, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_secs(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// Effects that just records calls. The real adapter (gt sling, rotation chain) is an
/// edge follow-up; what 6.e proves is the wiring up to the boundary.
#[derive(Clone, Default)]
struct RecordingEffects {
    slings: Arc<Mutex<Vec<(String, String)>>>,
    rotations: Arc<Mutex<Vec<String>>>,
}

impl Effects for RecordingEffects {
    fn sling(&self, convoy: &str, member: &str) {
        self.slings
            .lock()
            .unwrap()
            .push((convoy.into(), member.into()));
    }
    fn rotate(&self, account: &str) {
        self.rotations.lock().unwrap().push(account.into());
    }
}

/// Poll the audit log until `pred` says it's ready (or timeout). The writer keeps appending
/// while the loop drains relays; we read without a shared lock and tolerate a momentary
/// half-written line as a transient parse error → retry.
async fn wait_for(
    log: &Path,
    pred: impl Fn(&[EventRecord]) -> bool,
    timeout: Duration,
) -> Vec<EventRecord> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(recs) = read_all(log) {
            if pred(&recs) {
                return recs;
            }
        }
        if Instant::now() >= deadline {
            let recs = read_all(log).unwrap_or_default();
            let kinds: Vec<&str> = recs.iter().map(|r| r.kind.as_str()).collect();
            panic!("timeout; log so far ({} records): {:?}", recs.len(), kinds);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn filter(records: &[EventRecord], prefix: &str) -> Vec<EventRecord> {
    records
        .iter()
        .filter(|r| r.kind.starts_with(prefix))
        .cloned()
        .collect()
}

#[tokio::test]
async fn multi_domain_flow_through_root_replays_byte_identical() {
    // --- setup -------------------------------------------------------------------------
    let repo = Arc::new(InMemoryBeads::default());
    repo.upsert(&Bead::new("b1", "work", BeadStatus::Pending, 1))
        .await
        .unwrap();

    let log_path = std::env::temp_dir().join(format!(
        "gt-root-{}-{}.events.jsonl",
        std::process::id(),
        ulid::Ulid::new()
    ));
    let _ = std::fs::remove_file(&log_path);

    let clock = ManualClock::new(10);
    let effects = RecordingEffects::default();
    let effects_view = effects.clone();

    let root = spawn(
        repo.clone(),
        effects,
        clock.clone(),
        &log_path,
        RootConfig {
            capacity: 1,
            ..RootConfig::default()
        },
    );

    // --- scheduling + patrol + agent: stale lease + reclaim ----------------------------
    // T=10s: enqueue. The reactor will record the priority for later re-enqueue.
    root.sched.enqueue("b1", 1).await;

    // Wait until the lease has been opened (Dispatched → reactor → patrol.register →
    // PatrolEvent::LeaseRegistered in the log). Polling the log is the source of truth, like
    // in production.
    wait_for(
        &log_path,
        |recs| recs.iter().any(|r| r.kind == "patrol.lease_registered"),
        Duration::from_secs(3),
    )
    .await;

    // T=45s: never heartbeat → tick beyond the timeout. The patrol detects the expiry, the
    // reactor reclaims the bead, the scheduler re-dispatches.
    clock.set(10 + LEASE_TIMEOUT + 5);
    root.patrol.tick(clock.now_secs(), LEASE_TIMEOUT).await;

    // Wait for the second dispatch — the reclaim worked end to end.
    let recs = wait_for(
        &log_path,
        |recs| {
            recs.iter()
                .filter(|r| r.kind == "scheduling.dispatched")
                .count()
                >= 2
        },
        Duration::from_secs(3),
    )
    .await;

    // Two distinct workers: the doomed one and the fresh one.
    let workers: Vec<String> = recs
        .iter()
        .filter(|r| r.kind == "scheduling.dispatched")
        .filter_map(|r| r.decode::<SchedEvent>().ok())
        .filter_map(|e| match e {
            SchedEvent::Dispatched { worker, .. } => Some(worker),
            _ => None,
        })
        .collect();
    assert_eq!(workers.len(), 2);
    assert_ne!(workers[0], workers[1]);

    // Drive the completion edge: agent transitions + repo done + close the lease.
    root.agent_events
        .send(Envelope::root(AgentEvent::Spawned {
            session: "b1".into(),
            rig: "granite".into(),
        }))
        .await
        .unwrap();
    let mut b = repo.get("b1").await.unwrap().unwrap();
    b.status = BeadStatus::Done;
    repo.upsert(&b).await.unwrap();
    root.patrol.close("b1").await;
    root.agent_events
        .send(Envelope::root(AgentEvent::SessionEnd {
            session: "b1".into(),
        }))
        .await
        .unwrap();
    root.sched.capacity_freed().await;

    // --- orchestration: one-member convoy through the handoff --------------------------
    root.orch
        .create_convoy("c1", vec!["b1".to_string()])
        .await;
    root.orch.launch("c1").await;

    // Wait for the convoy handoff to reach Effects::sling and the convoy to close.
    wait_for(
        &log_path,
        |recs| recs.iter().any(|r| r.kind == "orch.member_dispatched"),
        Duration::from_secs(3),
    )
    .await;
    assert_eq!(
        effects_view.slings.lock().unwrap().as_slice(),
        &[("c1".to_string(), "b1".to_string())],
        "MemberDispatched must reach the Effects::sling adapter",
    );
    root.orch.member_done("c1", "b1").await;

    // Wait for the terminal markers in the log: convoy closed + an agent session_end.
    let records = wait_for(
        &log_path,
        |recs| {
            recs.iter().any(|r| r.kind == "orch.convoy_closed")
                && recs.iter().any(|r| r.kind == "agent.session_end")
        },
        Duration::from_secs(3),
    )
    .await;

    // --- gate fact: unified replay == per-domain prefix replay -------------------------
    let sched_indep = replay(&filter(&records, "scheduling."), SchedState::default(), |s, e: &SchedEvent| s.apply(e)).unwrap();
    let patrol_indep = replay(&filter(&records, "patrol."), PatrolState::default(), |s, e: &PatrolEvent| s.apply(e)).unwrap();
    let orch_indep = replay(&filter(&records, "orch."), OrchState::default(), |s, e: &OrchEvent| s.apply(e)).unwrap();
    let merge_indep = replay(&filter(&records, "merge."), MergeState::default(), |s, e: &MergeEvent| s.apply(e)).unwrap();
    let quota_indep = replay(&filter(&records, "quota."), QuotaState::default(), |s, e: &QuotaEvent| s.apply(e)).unwrap();
    let agent_indep = replay(&filter(&records, "agent."), SessionRegistry::default(), |s, e: &AgentEvent| s.apply(e)).unwrap();

    let unified = replay_gt(&records).unwrap();

    assert_eq!(unified.sched, sched_indep, "unified sched must match prefix-replayed sched");
    assert_eq!(unified.patrol, patrol_indep, "unified patrol must match prefix-replayed patrol");
    assert_eq!(unified.orch, orch_indep, "unified orch must match prefix-replayed orch");
    assert_eq!(unified.merge, merge_indep, "unified merge must match prefix-replayed merge");
    assert_eq!(unified.quota, quota_indep, "unified quota must match prefix-replayed quota");
    assert_eq!(
        unified.agent.fingerprint(),
        agent_indep.fingerprint(),
        "unified agent registry must rebuild to the same fingerprint as the prefix replay",
    );

    // Trajectory sanity from the unified state.
    assert_eq!(unified.sched.dispatched.len(), 2, "doomed + fresh worker");
    assert_eq!(unified.patrol.expired.len(), 1, "exactly one lease expired");
    assert!(unified.patrol.tracker.is_empty(), "after close + expire the tracker is empty");

    // The convoy ran to its terminator.
    assert!(
        records.iter().any(|r| r.kind == "orch.convoy_closed"),
        "convoy must close",
    );

    // No reaction error and no undecodable event.
    assert_eq!(root.dead_letters(), 0, "no dead-letters expected on a clean run");

    // Sanity: every record decodes through GtEvent::from_record (no unknown prefix landed).
    for r in &records {
        let _ = gt_root::GtEvent::from_record(r)
            .unwrap_or_else(|e| panic!("record {} did not decode through GtEvent: {e}", r.kind));
    }

    let _ = std::fs::remove_file(&log_path);
    root.shutdown();
}
