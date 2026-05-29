//! hq-mc72.12 C2 Mayor gate: the periodic mayor scan files a Pending delegation for every
//! launched convoy not already tracked. Replicates one iteration of
//! `bins/gt/src/main.rs::spawn_mayor_loop` so the assertion is deterministic.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gt_beads::InMemoryBeads;
use gt_mayor::DelegationStatus;
use gt_polecat::{FakeTmux, PolecatLifecycle, PolecatSupervisor, RestartConfig, SpawnTemplate};
use gt_root::{spawn, RealEffects, RootConfig, SystemClock};

#[tokio::test]
async fn launched_convoy_is_auto_delegated_idempotently() {
    let dir = tempdir();
    let log = dir.join("events.jsonl");

    let lifecycle = PolecatLifecycle::new(Box::new(FakeTmux::new()), test_template());
    let supervisor = Arc::new(PolecatSupervisor::new(
        Arc::new(FakeTmux::new()),
        RestartConfig::default(),
        u32::MAX,
    ));
    let (effects, quota_slot) = RealEffects::new(lifecycle, supervisor);
    let root = spawn(
        Arc::new(InMemoryBeads::default()),
        Arc::new(gt_merge::InMemoryMergeRepo::default()),
        Arc::new(gt_patrol::InMemoryPatrolRepo::default()),
        Arc::new(gt_orchestration::InMemoryOrchRepo::default()),
        effects,
        SystemClock,
        &log,
        RootConfig::default(),
    );
    let _ = quota_slot.set(root.quota.clone());

    // Stage + launch a convoy so it enters ConvoyState::Launched.
    root.orch
        .create_convoy("c-mayor", vec!["m1".to_string(), "m2".to_string()])
        .await;
    root.orch.launch("c-mayor").await;

    // Wait until orch snapshot reports the convoy as launched.
    let convoys = wait_for_convoys(Duration::from_secs(3), &root, |cs| {
        cs.iter().any(|c| {
            c.id == "c-mayor"
                && matches!(c.state, gt_orchestration::ConvoyState::Launched)
        })
    })
    .await;
    assert!(convoys, "convoy never reached Launched state");

    // One mayor scan iteration (the loop body in spawn_mayor_loop).
    mayor_scan(&root).await;

    // Mayor state should now hold the Pending delegation keyed by convoy id.
    let state = root.mayor.snapshot().await;
    let delegation = state
        .delegations
        .get("convoy-c-mayor")
        .expect("convoy delegation present after first scan");
    assert_eq!(delegation.status, DelegationStatus::Pending);
    assert_eq!(delegation.target, "ops");

    // Idempotency: a second scan must not produce a second delegation event (re-delegating
    // an existing id would error). State stays at exactly one entry.
    let count_before = state.delegations.len();
    mayor_scan(&root).await;
    let state2 = root.mayor.snapshot().await;
    assert_eq!(state2.delegations.len(), count_before, "scan was not idempotent");

    // Audit log holds the canonical mayor.delegated record.
    let log_path = root.log_path().to_path_buf();
    let saw_delegated = wait_for(Duration::from_secs(3), || {
        gt_audit::read_all(&log_path)
            .map(|recs| {
                recs.iter().any(|r| {
                    r.kind == "mayor.delegated"
                        && matches!(
                            r.decode::<gt_mayor::MayorEvent>(),
                            Ok(gt_mayor::MayorEvent::Delegated { ref id, .. })
                                if id == "convoy-c-mayor"
                        )
                })
            })
            .unwrap_or(false)
    })
    .await;
    assert!(saw_delegated, "mayor.delegated record missing from audit log");

    root.shutdown();
}

/// One iteration of the mayor scan — identical to the loop body in `spawn_mayor_loop`.
async fn mayor_scan<R>(root: &gt_root::RootHandle<R>)
where
    R: gt_beads::BeadRepository + Clone + 'static,
{
    let convoys = root.orch.snapshot().await;
    let mayor_state = root.mayor.snapshot().await;
    for convoy in convoys {
        if !matches!(convoy.state, gt_orchestration::ConvoyState::Launched) {
            continue;
        }
        let id = format!("convoy-{}", convoy.id);
        if mayor_state.delegations.contains_key(&id) {
            continue;
        }
        let summary = format!(
            "convoy {} launched ({} members)",
            convoy.id,
            convoy.members.len()
        );
        let _ = gt_mayor::mayor::delegate(&root.mayor, id, "ops", summary).await;
    }
}

fn test_template() -> SpawnTemplate {
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
    p.push(format!("gt-mayor-loop-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

async fn wait_for(timeout: Duration, mut pred: impl FnMut() -> bool) -> bool {
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

async fn wait_for_convoys<R, F>(timeout: Duration, root: &gt_root::RootHandle<R>, mut pred: F) -> bool
where
    R: gt_beads::BeadRepository + Clone + 'static,
    F: FnMut(&[gt_orchestration::Convoy]) -> bool,
{
    let deadline = Instant::now() + timeout;
    loop {
        let snap = root.orch.snapshot().await;
        if pred(&snap) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}
