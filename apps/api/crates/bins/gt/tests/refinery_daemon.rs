//! hq-mc72.12 C2 gate: Refinery MERGE_READY runs as a **live supervised daemon** in the
//! composition root. Emit a `MergeReadyPayload` to a `gt_channel::Channel`; the supervised
//! `gt_merge::refinery::run` (under `gt_polecat::supervise_daemon`) picks it up, drives
//! `merge.submit`, and a `merge.ready` record lands in the audit log. Mirrors the
//! bin-level wiring `main.rs::spawn_refinery_daemon` does in production.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gt_beads::InMemoryBeads;
use gt_channel::Channel;
use gt_merge::MergeReadyPayload;
use gt_polecat::{
    supervise_daemon, FakeTmux, PolecatLifecycle, RestartConfig, RestartTracker, SpawnTemplate,
};
use gt_root::{spawn, RealEffects, RootConfig, SystemClock};

#[tokio::test]
async fn supervised_refinery_consumes_channel_event_and_logs_merge_ready() {
    let dir = tempdir();
    let log = dir.join("events.jsonl");
    let channel = Channel::open(&dir, "merge-ready").expect("open channel");

    // Effects: real adapter with a fake-tmux lifecycle (we do not sling in this test, just
    // need a valid RealEffects).
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

    // Spawn the supervised refinery daemon — same shape as main.rs::spawn_refinery_daemon.
    let merge = root.merge.clone();
    let channel_for_task = channel.clone();
    let daemon = tokio::spawn(async move {
        let mut tracker = RestartTracker::new(RestartConfig::default());
        let make = move || {
            let channel = channel_for_task.clone();
            let merge = merge.clone();
            async move {
                let _ = gt_merge::refinery::run(channel, merge).await;
            }
        };
        supervise_daemon("refinery", make, &mut tracker, u32::MAX, || 0).await;
    });

    // Emit a MERGE_READY message — the supervised daemon must drain it and `merge.submit`
    // it, producing a `merge.ready` event in the audit log.
    let payload = serde_json::to_vec(&MergeReadyPayload {
        bead: "hq-test-1".to_string(),
        branch: "feat/test".to_string(),
    })
    .unwrap();
    channel.emit(&payload).expect("emit channel msg");

    let log_path = root.log_path().to_path_buf();
    let saw_ready = wait_for_predicate(Duration::from_secs(5), || {
        gt_audit::read_all(&log_path)
            .map(|recs| recs.iter().any(|r| r.kind == "merge.ready"))
            .unwrap_or(false)
    })
    .await;

    daemon.abort();
    let _ = daemon.await;
    root.shutdown();

    assert!(
        saw_ready,
        "supervised refinery did not turn the channel event into a `merge.ready` record",
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
    p.push(format!("gt-refinery-daemon-{}", ulid::Ulid::new()));
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
