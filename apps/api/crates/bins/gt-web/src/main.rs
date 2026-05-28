//! `gt-web` binary — the read-side process. Boots the composition root (`bins/gt`) plus the
//! Axum router; the single tokio runtime lives here per `docs/01-architecture.md`.
//!
//! Persistence (hq-j9ou): when `GT_DOLT_URL` is set, the bead repo behind `AppState.beads`
//! and behind the root is the real Dolt-backed `DoltBeads`; otherwise the bin keeps the
//! in-memory port so the API stays runnable on a host without Dolt. `GT_PG_AUDIT_URL`
//! mirrors every appended `EventRecord` into Postgres (canonical audit per docs/04).

use std::sync::Arc;

use gt_agent::InMemorySessions;
use gt_beads::{BeadRepository, InMemoryBeads};
use gt_root::{spawn, LogEffects, RootConfig, RootHandle, SystemClock};
use gt_store_dolt::DoltBeads;
use gt_store_pg::PgAudit;
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

        match std::env::var("GT_DOLT_URL").ok() {
            Some(url) => {
                let dolt = DoltBeads::connect(&url).expect("connect Dolt");
                dolt.ensure_schema().await.expect("Dolt ensure_schema");
                eprintln!("[gt-web] beads: Dolt @ {url}");
                serve(Arc::new(dolt), &log_path, &bind).await;
            }
            None => {
                eprintln!("[gt-web] beads: in-memory (set GT_DOLT_URL for Dolt persistence)");
                serve(Arc::new(InMemoryBeads::default()), &log_path, &bind).await;
            }
        }
    });
}

async fn serve<R>(beads: Arc<R>, log_path: &str, bind: &str)
where
    R: BeadRepository + Send + Sync + 'static,
    Arc<R>: BeadRepository + Clone + 'static,
{
    let sessions = Arc::new(InMemorySessions::default());

    let root = spawn(
        beads.clone(),
        LogEffects,
        SystemClock,
        log_path,
        RootConfig::default(),
    );

    let audit_task = spawn_pg_audit(&root).await;

    let state = AppState {
        beads,
        sessions,
        agent_events: root.agent_events.clone(),
        events: root.events_sender(),
    };

    let app = router(state);
    let listener = tokio::net::TcpListener::bind(bind).await.expect("bind gt-web");
    eprintln!(
        "[gt-web] up on {bind} — event log: {}",
        root.log_path().display()
    );

    let _ = axum::serve(listener, app).await;
    if let Some(task) = audit_task {
        task.abort();
    }
    root.shutdown();
}

async fn spawn_pg_audit<R>(root: &RootHandle<R>) -> Option<tokio::task::JoinHandle<()>>
where
    R: BeadRepository + Clone + 'static,
{
    let url = std::env::var("GT_PG_AUDIT_URL").ok()?;
    let audit = match PgAudit::connect(&url).await {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[gt-web] PG audit disabled — connect failed: {e}");
            return None;
        }
    };
    if let Err(e) = gt_store_pg::ensure_schema(audit.pool()).await {
        eprintln!("[gt-web] PG audit disabled — migrations failed: {e}");
        return None;
    }
    eprintln!("[gt-web] audit: Postgres @ {url}");
    let mut rx = root.subscribe_events();
    Some(tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(rec) => {
                    if let Err(e) = audit.append(&rec).await {
                        eprintln!("[gt-web] PG audit append failed ({}): {e}", rec.kind);
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    eprintln!("[gt-web] PG audit lagged by {n} events (catching up)");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    }))
}
