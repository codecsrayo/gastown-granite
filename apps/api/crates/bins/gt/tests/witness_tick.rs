//! hq-mc72.12 C2 Witness tick gate: a watched target ticked past its threshold raises an
//! escalation, and the reactor's hq-mysw arm turns that into a status bead. This is the
//! pipeline the periodic ticker (`bins/gt/src/main.rs::spawn_witness_ticker`) drives on a
//! timer; here we invoke the tick directly so the assertion is deterministic.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gt_beads::{BeadRepository, InMemoryBeads};
use gt_polecat::{FakeTmux, PolecatLifecycle, SpawnTemplate};
use gt_root::{spawn, RealEffects, RootConfig, SystemClock};

#[tokio::test]
async fn watched_target_ticked_past_threshold_escalates_and_files_bead() {
    let dir = tempdir();
    let log = dir.join("events.jsonl");

    let lifecycle = PolecatLifecycle::new(Box::new(FakeTmux::new()), template());
    let (effects, quota_slot) = RealEffects::new(lifecycle, test_polecat_supervisor());
    let repo = Arc::new(InMemoryBeads::default());
    let root = spawn(
        repo.clone(),
        Arc::new(gt_merge::InMemoryMergeRepo::default()),
        Arc::new(gt_patrol::InMemoryPatrolRepo::default()),
        Arc::new(gt_orchestration::InMemoryOrchRepo::default()),
        effects,
        SystemClock,
        &log,
        RootConfig::default(),
    );
    let _ = quota_slot.set(root.quota.clone());

    // Watch a worker with a 1s inactivity budget, then tick well past it — what the periodic
    // ticker does each interval. The actor raises EscalationRaised; the reactor turns it into
    // an escalation-<worker> status bead (hq-mysw arm).
    gt_witness::witness::watch(&root.witness, "polecat-stuck", 0, 1)
        .await
        .expect("watch");
    gt_witness::witness::tick(&root.witness, "polecat-stuck", 1_000)
        .await
        .expect("tick");

    let log_path = root.log_path().to_path_buf();
    let saw_escalation = wait_for_predicate(Duration::from_secs(5), || {
        gt_audit::read_all(&log_path)
            .map(|recs| recs.iter().any(|r| r.kind == "witness.escalation_raised"))
            .unwrap_or(false)
    })
    .await;
    let saw_bead = wait_for_async(Duration::from_secs(5), || {
        let repo = repo.clone();
        async move {
            repo.get("escalation-polecat-stuck")
                .await
                .unwrap_or(None)
                .is_some()
        }
    })
    .await;

    root.shutdown();

    assert!(saw_escalation, "tick past threshold did not raise witness.escalation_raised");
    assert!(saw_bead, "escalation did not produce an escalation-<worker> status bead");
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
    p.push(format!("gt-witness-tick-{}", ulid::Ulid::new()));
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

async fn wait_for_async<F, Fut>(timeout: Duration, mut pred: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + timeout;
    loop {
        if pred().await {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}
