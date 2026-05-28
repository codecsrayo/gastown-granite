//! `gt` binary — the thin boot. Per `docs/01-architecture.md`, the **single** tokio runtime
//! is created here, in `bins/`; the domain crates never create one, they receive handles.
//!
//! Wires the in-memory bead repo + the real subprocess/rotation effects (`gt sling` child
//! processes + the predictive rotation chain). The Dolt-backed repo (`gt-store-dolt`) slots in
//! here without touching the composition wiring.

use std::path::PathBuf;
use std::sync::Arc;

use gt_beads::InMemoryBeads;
use gt_root::{spawn, RealEffects, RootConfig, SystemClock};

fn main() {
    // One runtime for the whole process.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    runtime.block_on(async {
        let log_path = std::env::var("GT_EVENT_LOG")
            .unwrap_or_else(|_| "/tmp/gt.events.jsonl".to_string());
        let gt_bin = std::env::var("GT_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("gt"));

        let repo = Arc::new(InMemoryBeads::default());
        let (effects, quota_slot) = RealEffects::new(gt_bin);
        let root = spawn(repo, effects, SystemClock, &log_path, RootConfig::default());
        let _ = quota_slot.set(root.quota.clone());

        eprintln!("[gt] composition root up — event log: {}", root.log_path().display());
        eprintln!("[gt] (edges: scheduler/patrol/merge/quota/orchestration actors live; drive via handles)");

        // Idle until interrupted: the actors and the draining loop run in the background;
        // the real edges (timers, probes, the channel watcher, the API) push work in.
        let _ = tokio::signal::ctrl_c().await;
        eprintln!("[gt] shutting down (dead-letters: {})", root.dead_letters());
        root.shutdown();
    });
}
