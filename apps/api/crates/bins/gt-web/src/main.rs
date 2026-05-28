//! `gt-web` binary — the read-side process. Boots the composition root (`bins/gt`) plus the
//! Axum router; the single tokio runtime lives here per `docs/01-architecture.md`.

use std::path::PathBuf;
use std::sync::Arc;

use gt_agent::InMemorySessions;
use gt_audit::JsonlWriter;
use gt_beads::InMemoryBeads;
use gt_root::{spawn, RealEffects, RootConfig, SystemClock};
use gt_web::{router, AppState, AuthConfig, JsonlWebAudit, WebAuditSink};

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    runtime.block_on(async {
        let log_path = std::env::var("GT_EVENT_LOG")
            .unwrap_or_else(|_| "/tmp/gt.events.jsonl".to_string());
        let bind = std::env::var("GT_WEB_BIND").unwrap_or_else(|_| "127.0.0.1:8787".to_string());
        let gt_bin = std::env::var("GT_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("gt"));

        // IAM at the frontier (doc 07). Fail-closed: refuse to start without a token unless
        // GT_WEB_AUTH=disabled is set explicitly (intended for in-tree dev only).
        let auth = match std::env::var("GT_WEB_TOKEN").ok() {
            Some(t) if !t.is_empty() => AuthConfig::bearer(t),
            _ if std::env::var("GT_WEB_AUTH").as_deref() == Ok("disabled") => {
                eprintln!("[gt-web] WARNING: auth disabled via GT_WEB_AUTH=disabled");
                AuthConfig::open()
            }
            _ => {
                eprintln!("[gt-web] refusing to start: set GT_WEB_TOKEN or GT_WEB_AUTH=disabled");
                std::process::exit(2);
            }
        };

        // In-memory adapters keep the bin runnable end-to-end before the Dolt-backed read-side
        // lands. Real adapters slot in here without touching the router.
        let beads = Arc::new(InMemoryBeads::default());
        let sessions = Arc::new(InMemorySessions::default());

        let (effects, quota_slot) = RealEffects::new(gt_bin);
        let root = spawn(
            beads.clone(),
            effects,
            SystemClock,
            &log_path,
            RootConfig::default(),
        );
        let _ = quota_slot.set(root.quota.clone());

        // Frontier audit writes to the same gt.events.jsonl the reactor appends to — the
        // boundary's who-consulted-what records share the system log.
        let writer: Arc<dyn gt_audit::EventStore + Send + Sync> =
            Arc::new(JsonlWriter::new(root.log_path()));
        let audit: Arc<dyn WebAuditSink> = Arc::new(JsonlWebAudit::new(writer));

        let state = AppState {
            beads,
            sessions,
            agent_events: root.agent_events.clone(),
            events: root.events_sender(),
        };

        let app = router(state, auth, audit);
        let listener = tokio::net::TcpListener::bind(&bind).await.expect("bind gt-web");
        eprintln!("[gt-web] up on {bind} — event log: {}", root.log_path().display());

        let _ = axum::serve(listener, app).await;
        root.shutdown();
    });
}
