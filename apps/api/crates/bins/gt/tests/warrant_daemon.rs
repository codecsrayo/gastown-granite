//! hq-mc72.12 C8 gate: an operator emits a warrant onto the gt-channel; the supervised
//! warrant daemon (under `gt_polecat::supervise_daemon`) decodes it and upserts a
//! `BeadStatus::Failed` escalation row keyed `escalation-<worker>`. Mirrors the
//! `bins/gt/src/main.rs::spawn_warrant_daemon` wire — the helpers there are bin-private so
//! the test replicates the supervised shape, exactly as `refinery_daemon.rs` does.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gt_beads::{Bead, BeadRepository, BeadStatus, InMemoryBeads};
use gt_channel::Channel;
use gt_polecat::{supervise_daemon, RestartConfig, RestartTracker};

#[tokio::test]
async fn warrant_message_becomes_escalation_bead() {
    let dir = tempdir();
    let channel = Channel::open(&dir, "warrant").expect("open channel");
    let repo = Arc::new(InMemoryBeads::default());

    // Spawn the supervised warrant watcher with the same shape `spawn_warrant_daemon` uses.
    let repo_for_task = repo.clone();
    let channel_for_task = channel.clone();
    let daemon = tokio::spawn(async move {
        let mut tracker = RestartTracker::new(RestartConfig::default());
        let make = move || {
            let channel = channel_for_task.clone();
            let repo = repo_for_task.clone();
            async move {
                let Ok(mut rx) = channel.subscribe(64) else { return };
                while let Some(msg) = rx.recv().await {
                    if let Ok(req) = serde_json::from_slice::<TestWarrant>(&msg.payload) {
                        let bead = Bead::new(
                            format!("escalation-{}", req.worker),
                            format!("Warrant filed: worker {} ({})", req.worker, req.reason),
                            BeadStatus::Failed,
                            0,
                        );
                        let _ = repo.upsert(&bead).await;
                    }
                    let _ = channel.ack(&msg);
                }
            }
        };
        supervise_daemon("warrant", make, &mut tracker, u32::MAX, || 0).await;
    });

    // Emit a warrant — supervised daemon must upsert the escalation row.
    let payload =
        serde_json::to_vec(&serde_json::json!({"worker":"alice","reason":"unresponsive"}))
            .unwrap();
    channel.emit(&payload).expect("emit warrant");

    let saw = wait_for_async(Duration::from_secs(5), || {
        let repo = repo.clone();
        async move {
            repo.get("escalation-alice")
                .await
                .unwrap_or(None)
                .is_some()
        }
    })
    .await;

    daemon.abort();
    let _ = daemon.await;

    assert!(saw, "supervised warrant daemon did not file an escalation bead");
    let bead = repo
        .get("escalation-alice")
        .await
        .unwrap()
        .expect("bead present");
    assert_eq!(bead.status, BeadStatus::Failed);
    assert_eq!(bead.priority, 0);
    assert!(bead.title.contains("unresponsive"), "reason carried into title");
}

#[tokio::test]
async fn malformed_warrant_is_acked_and_dropped() {
    let dir = tempdir();
    let channel = Channel::open(&dir, "warrant").expect("open channel");
    let repo = Arc::new(InMemoryBeads::default());

    let repo_for_task = repo.clone();
    let channel_for_task = channel.clone();
    let daemon = tokio::spawn(async move {
        let mut tracker = RestartTracker::new(RestartConfig::default());
        let make = move || {
            let channel = channel_for_task.clone();
            let repo = repo_for_task.clone();
            async move {
                let Ok(mut rx) = channel.subscribe(64) else { return };
                while let Some(msg) = rx.recv().await {
                    if let Ok(req) = serde_json::from_slice::<TestWarrant>(&msg.payload) {
                        let bead = Bead::new(
                            format!("escalation-{}", req.worker),
                            req.reason,
                            BeadStatus::Failed,
                            0,
                        );
                        let _ = repo.upsert(&bead).await;
                    }
                    let _ = channel.ack(&msg);
                }
            }
        };
        supervise_daemon("warrant", make, &mut tracker, u32::MAX, || 0).await;
    });

    // Garbage payload — must be acked, never produce a bead.
    channel.emit(b"not valid json").expect("emit");
    tokio::time::sleep(Duration::from_millis(200)).await;

    // No escalation row created — the channel directory should be empty (ack removed the file).
    let entries: Vec<_> = std::fs::read_dir(channel.dir())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s == "event")
                .unwrap_or(false)
        })
        .collect();

    daemon.abort();
    let _ = daemon.await;

    assert!(entries.is_empty(), "malformed warrant was not acked");
}

#[derive(serde::Deserialize)]
struct TestWarrant {
    worker: String,
    reason: String,
}

fn tempdir() -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("gt-warrant-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&p).unwrap();
    p
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
