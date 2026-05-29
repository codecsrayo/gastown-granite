//! hq-mc72.12 C2 Deacon gate: a Merge::Ready → Merged cycle drives `deacon.item_begun` and
//! `deacon.item_finished` in the audit log (reactor arms), and a `begin_drain` after the
//! merge finishes produces `deacon.drain_complete` (the pending set is empty at drain time
//! so the actor fires it in the same tick as `DrainRequested`). Mirrors the wire
//! `bins/gt/src/main.rs::drain_via_deacon` exercises on SIGTERM.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gt_beads::InMemoryBeads;
use gt_polecat::{FakeTmux, PolecatLifecycle, SpawnTemplate};
use gt_root::{spawn, RealEffects, RootConfig, SystemClock};

#[tokio::test]
async fn merge_cycle_drives_deacon_track_finish_and_drain_completes() {
    let dir = tempdir();
    let log = dir.join("events.jsonl");

    let lifecycle = PolecatLifecycle::new(Box::new(FakeTmux::new()), template());
    let (effects, quota_slot) = RealEffects::new(lifecycle, test_polecat_supervisor());
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

    // Drive a merge slot lifecycle: Submit -> Ready (reactor arms deacon.track) -> Complete
    // -> Merged (reactor arms deacon.finish).
    root.merge.submit("hq-deacon-1", "feat/test", "msg-1").await;
    let saw_begun = wait_for_predicate(Duration::from_secs(3), || {
        any_kind(root.log_path(), "deacon.item_begun")
    })
    .await;
    assert!(saw_begun, "Merge::Ready reactor arm did not call deacon.track");

    root.merge
        .complete("hq-deacon-1", "deadbeefcafe000000000000000000000000")
        .await;
    let saw_finished = wait_for_predicate(Duration::from_secs(3), || {
        any_kind(root.log_path(), "deacon.item_finished")
    })
    .await;
    assert!(
        saw_finished,
        "Merge::Merged reactor arm did not call deacon.finish"
    );

    // Now trigger a drain — pending set is empty, so `DrainRequested` is immediately followed
    // by `DrainComplete` in the same exec tick.
    gt_deacon::deacon::begin_drain(&root.deacon, "test", 1000)
        .await
        .expect("begin_drain");
    let saw_complete = wait_for_predicate(Duration::from_secs(3), || {
        any_kind(root.log_path(), "deacon.drain_complete")
    })
    .await;
    assert!(
        saw_complete,
        "deacon.drain_complete did not appear after begin_drain on empty pending",
    );

    root.shutdown();
}

#[tokio::test]
async fn drain_during_in_flight_merge_waits_then_completes_on_finish() {
    let dir = tempdir();
    let log = dir.join("events.jsonl");

    let lifecycle = PolecatLifecycle::new(Box::new(FakeTmux::new()), template());
    let (effects, quota_slot) = RealEffects::new(lifecycle, test_polecat_supervisor());
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

    // Open a merge slot — Ready arm tracks it in the deacon.
    root.merge.submit("hq-inflight-1", "feat/x", "msg-x").await;
    let _ = wait_for_predicate(Duration::from_secs(3), || {
        any_kind(root.log_path(), "deacon.item_begun")
    })
    .await;

    // Drain while the merge is still in-flight: actor records DrainRequested but does NOT
    // emit DrainComplete yet (pending is non-empty).
    gt_deacon::deacon::begin_drain(&root.deacon, "test", 1000)
        .await
        .expect("begin_drain");

    let saw_complete_early = wait_short(Duration::from_millis(200), || {
        any_kind(root.log_path(), "deacon.drain_complete")
    })
    .await;
    assert!(
        !saw_complete_early,
        "drain_complete fired while a merge was still in flight",
    );

    // Complete the merge → finish + DrainComplete in the same tick.
    root.merge
        .complete("hq-inflight-1", "deadbeef00000000000000000000000000000000")
        .await;
    let saw_complete = wait_for_predicate(Duration::from_secs(3), || {
        any_kind(root.log_path(), "deacon.drain_complete")
    })
    .await;
    assert!(
        saw_complete,
        "deacon.drain_complete did not appear after the in-flight merge finished",
    );

    root.shutdown();
}

fn any_kind(path: &std::path::Path, kind: &str) -> bool {
    gt_audit::read_all(path)
        .map(|recs| recs.iter().any(|r| r.kind == kind))
        .unwrap_or(false)
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

fn test_polecat_supervisor() -> Arc<gt_polecat::PolecatSupervisor> {
    Arc::new(gt_polecat::PolecatSupervisor::new(
        Arc::new(FakeTmux::new()),
        gt_polecat::RestartConfig::default(),
        u32::MAX,
    ))
}

fn tempdir() -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("gt-deacon-drain-{}", ulid::Ulid::new()));
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

/// Wait the full `timeout` for `pred` to become true; used to assert a *negative* condition
/// stably (the value remains false for the whole window).
async fn wait_short(timeout: Duration, mut pred: impl FnMut() -> bool) -> bool {
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
