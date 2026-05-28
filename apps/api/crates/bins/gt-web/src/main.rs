//! `gt-web` binary — the read-side process. Boots the composition root (`bins/gt`) plus the
//! Axum router; the single tokio runtime lives here per `docs/01-architecture.md`.

use std::sync::Arc;

use gt_agent::InMemorySessions;
use gt_beads::InMemoryBeads;
use gt_root::{spawn, LogEffects, RootConfig, SystemClock};
use gt_web::{router, AppState};

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    runtime.block_on(async {
        let log_path = std::env::var("GT_EVENT_LOG")
            .unwrap_or_else(|_| "/tmp/gt.events.jsonl".to_string());
        let bind = std::env::var("GT_WEB_BIND").unwrap_or_else(|_| "127.0.0.1:8787".to_string());

        // In-memory adapters keep the bin runnable end-to-end before the Dolt-backed read-side
        // lands. Real adapters slot in here without touching the router.
        let beads = Arc::new(InMemoryBeads::default());
        let sessions = Arc::new(InMemorySessions::default());

        let root = spawn(
            beads.clone(),
            LogEffects,
            SystemClock,
            &log_path,
            RootConfig::default(),
        );

        let state = AppState {
            beads,
            sessions,
            agent_events: root.agent_events.clone(),
            events: root.events_sender(),
        };

        let app = router(state);
        let listener = tokio::net::TcpListener::bind(&bind).await.expect("bind gt-web");
        eprintln!("[gt-web] up on {bind} — event log: {}", root.log_path().display());

        let _ = axum::serve(listener, app).await;
        root.shutdown();
    });
}
