//! hq-mc72.12 C2 Witness gate: a `PatrolEvent::LeaseExpired` automatically arms a witness
//! watch on the dead worker (root reactor arm). Surfaces in the audit log as a
//! `witness.target_watched` record. Asserts the cross-domain wire without driving a full
//! sched → dispatch flow.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gt_beads::InMemoryBeads;
use gt_polecat::{FakeTmux, PolecatLifecycle, SpawnTemplate};
use gt_root::{spawn, RealEffects, RootConfig, SystemClock};

#[tokio::test]
async fn patrol_lease_expiry_auto_arms_witness_watch() {
    let dir = tempdir();
    let log = dir.join("events.jsonl");

    let lifecycle = PolecatLifecycle::new(Box::new(FakeTmux::new()), template());
    let (effects, quota_slot) = RealEffects::new(lifecycle);
    let repo = Arc::new(InMemoryBeads::default());
    let root = spawn(
        repo,
        Arc::new(gt_merge::InMemoryMergeRepo::default()),
        Arc::new(gt_patrol::InMemoryPatrolRepo::default()),
        Arc::new(gt_orchestration::InMemoryOrchRepo::default()),
        effects,
        SystemClock,
        &log,
        RootConfig::default(),
    );
    let _ = quota_slot.set(root.quota.clone());

    // Register a lease, then tick past the timeout so the patrol emits LeaseExpired. The
    // reactor's PatrolEvent::LeaseExpired arm then calls witness.watch on the dead worker.
    root.patrol.register("hq-test-1", "polecat-doomed", 1, 0).await;
    root.patrol.tick(10_000, 1).await;

    let log_path = root.log_path().to_path_buf();
    let saw_expired = wait_for_predicate(Duration::from_secs(5), || {
        gt_audit::read_all(&log_path)
            .map(|recs| recs.iter().any(|r| r.kind == "patrol.lease_expired"))
            .unwrap_or(false)
    })
    .await;
    let saw_watched = wait_for_predicate(Duration::from_secs(5), || {
        gt_audit::read_all(&log_path)
            .map(|recs| recs.iter().any(|r| r.kind == "witness.target_watched"))
            .unwrap_or(false)
    })
    .await;

    root.shutdown();

    assert!(saw_expired, "patrol did not emit lease_expired");
    assert!(
        saw_watched,
        "reactor arm did not auto-watch the dead worker through witness",
    );
}

fn template() -> SpawnTemplate {
    SpawnTemplate {
        rig: "test".to_string(),
        prefix: "gt".to_string(),
        workdir: std::env::temp_dir(),
        command: "sleep".to_string(),
        args: vec!["30".to_string()],
        base_env: vec![("GT_ROLE".to_string(), "polecat".to_string())],
        heartbeat_dir: std::env::temp_dir(),
    }
}

fn tempdir() -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("gt-witness-wire-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

async fn wait_for_predicate(timeout: Duration, mut pred: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if pred() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}
