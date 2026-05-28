//! `gt` binary — the thin boot. Per `docs/01-architecture.md`, the **single** tokio runtime
//! is created here, in `bins/`; the domain crates never create one, they receive handles.
//!
//! This wires the in-memory repo + the logging effects/system clock as a runnable skeleton.
//! Swapping in the Dolt adapter (`gt-store-dolt`) and the real subprocess/rotation effects is
//! an edge follow-up; the composition wiring they plug into is what this crate delivers.

use std::sync::Arc;

use gt_beads::InMemoryBeads;
use gt_root::{spawn, LogEffects, RootConfig, SystemClock};

fn main() {
    // One runtime for the whole process.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    runtime.block_on(async {
        let log_path = std::env::var("GT_EVENT_LOG")
            .unwrap_or_else(|_| "/tmp/gt.events.jsonl".to_string());

        let repo = Arc::new(InMemoryBeads::default());
        let root = spawn(
            repo,
            LogEffects,
            SystemClock,
            &log_path,
            RootConfig::default(),
        );

        eprintln!("[gt] composition root up — event log: {}", root.log_path().display());
        eprintln!("[gt] (edges: scheduler/patrol/merge/quota/orchestration actors live; drive via handles)");

        // Idle until interrupted: the actors and the draining loop run in the background;
        // the real edges (timers, probes, the channel watcher, the API) push work in.
        let _ = tokio::signal::ctrl_c().await;
        eprintln!("[gt] shutting down (dead-letters: {})", root.dead_letters());
        root.shutdown();
    });
}
