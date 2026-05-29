//! hq-mc72.12 C8 gate (estop): an operator emits an estop onto the gt-channel; the supervised
//! estop daemon decodes it and calls `gt_deacon::deacon::begin_drain`, producing
//! `deacon.drain_requested` in the audit log. Mirrors `bins/gt/src/main.rs::spawn_estop_daemon`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gt_beads::InMemoryBeads;
use gt_channel::Channel;
use gt_polecat::{supervise_daemon, FakeTmux, PolecatLifecycle, RestartConfig, RestartTracker, SpawnTemplate};
use gt_root::{spawn, RealEffects, RootConfig, SystemClock};

#[tokio::test]
async fn estop_message_triggers_deacon_drain() {
    let dir = tempdir();
    let log = dir.join("events.jsonl");
    let channel = Channel::open(&dir, "estop").expect("open channel");

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

    // Same shape as spawn_estop_daemon.
    let deacon = root.deacon.clone();
    let channel_for_task = channel.clone();
    let daemon = tokio::spawn(async move {
        let mut tracker = RestartTracker::new(RestartConfig::default());
        let make = move || {
            let channel = channel_for_task.clone();
            let deacon = deacon.clone();
            async move {
                let Ok(mut rx) = channel.subscribe(64) else { return };
                while let Some(msg) = rx.recv().await {
                    if let Ok(req) = serde_json::from_slice::<TestEstop>(&msg.payload) {
                        let _ = gt_deacon::deacon::begin_drain(&deacon, &req.by, 9999).await;
                    }
                    let _ = channel.ack(&msg);
                }
            }
        };
        supervise_daemon("estop", make, &mut tracker, u32::MAX, || 0).await;
    });

    let payload =
        serde_json::to_vec(&serde_json::json!({"by":"ops","reason":"runaway"})).unwrap();
    channel.emit(&payload).expect("emit estop");

    let log_path = root.log_path().to_path_buf();
    let saw_requested = wait_for_predicate(Duration::from_secs(5), || {
        gt_audit::read_all(&log_path)
            .map(|recs| recs.iter().any(|r| r.kind == "deacon.drain_requested"))
            .unwrap_or(false)
    })
    .await;
    // Pending is empty (no merges), so the BeginDrain-empty fix (hq-mc72.12.7) also fires
    // drain_complete in the same tick.
    let saw_complete = wait_for_predicate(Duration::from_secs(5), || {
        gt_audit::read_all(&log_path)
            .map(|recs| recs.iter().any(|r| r.kind == "deacon.drain_complete"))
            .unwrap_or(false)
    })
    .await;

    daemon.abort();
    let _ = daemon.await;
    root.shutdown();

    assert!(saw_requested, "estop did not produce deacon.drain_requested");
    assert!(
        saw_complete,
        "drain_complete expected (empty pending) — BeginDrain-empty path is the wire's value"
    );
}

#[derive(serde::Deserialize)]
struct TestEstop {
    by: String,
    #[allow(dead_code)]
    reason: String,
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
        RestartConfig::default(),
        u32::MAX,
    ))
}

fn tempdir() -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("gt-estop-{}", ulid::Ulid::new()));
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
