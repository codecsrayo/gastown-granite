//! `gt` binary — the thin boot. Per `docs/01-architecture.md`, the **single** tokio runtime
//! is created here, in `bins/`; the domain crates never create one, they receive handles.
//!
//! Persistence (hq-j9ou): when `GT_DOLT_URL` is set, the bead repo is the real Dolt-backed
//! `gt-store-dolt::DoltBeads`; otherwise the bin falls back to the in-memory port so host
//! runs without a Dolt server still work end-to-end. When `GT_PG_AUDIT_URL` is set, the
//! same boot also spawns the Postgres audit relay (canonical EventStore per docs/04).
//! The local `.events.jsonl` keeps writing in both modes as a spill/fallback.
//!
//! Effects (hq-7pdl.1): the composition root is wired with the production [`RealEffects`]
//! adapter (`gt sling` child processes + the `QuotaCommand::Rotate` chain). The `gt` binary
//! path is configurable via `GT_BIN`.

use std::path::PathBuf;
use std::sync::Arc;

use gt_beads::{BeadRepository, InMemoryBeads};
use gt_root::{spawn, RealEffects, RootConfig, RootHandle, SystemClock};
use gt_store_dolt::DoltBeads;
use gt_store_pg::PgAudit;
use gt_telemetry::{init as init_telemetry, TelemetryConfig};

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    // Telemetry: stderr fmt + OTLP/HTTP traces (if `OTEL_EXPORTER_OTLP_ENDPOINT`) + Prometheus
    // registry. Held until the runtime returns so the batch exporter flushes on shutdown.
    let _telemetry = init_telemetry(TelemetryConfig::from_env("gt"))
        .map_err(|e| eprintln!("[gt] telemetry init: {e} (continuing without exporter)"))
        .ok();

    runtime.block_on(async {
        let log_path = std::env::var("GT_EVENT_LOG")
            .unwrap_or_else(|_| "/tmp/gt.events.jsonl".to_string());
        let gt_bin = std::env::var("GT_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("gt"));

        match std::env::var("GT_DOLT_URL").ok() {
            Some(url) => {
                let dolt = DoltBeads::connect(&url).expect("connect Dolt");
                dolt.ensure_schema().await.expect("Dolt ensure_schema");
                eprintln!("[gt] beads: Dolt @ {url}");
                run(Arc::new(dolt), &log_path, gt_bin).await;
            }
            None => {
                eprintln!("[gt] beads: in-memory (set GT_DOLT_URL for Dolt persistence)");
                run(Arc::new(InMemoryBeads::default()), &log_path, gt_bin).await;
            }
        }
    });
}

async fn run<R>(repo: Arc<R>, log_path: &str, gt_bin: PathBuf)
where
    R: BeadRepository + 'static,
    Arc<R>: BeadRepository + Clone + 'static,
{
    let (effects, quota_slot) = RealEffects::new(gt_bin);
    let root = spawn(repo, effects, SystemClock, log_path, RootConfig::default());
    let _ = quota_slot.set(root.quota.clone());

    let audit_task = spawn_pg_audit(&root).await;

    eprintln!(
        "[gt] composition root up — event log: {}",
        root.log_path().display()
    );
    eprintln!("[gt] (edges: scheduler/patrol/merge/quota/orchestration actors live; drive via handles)");

    let _ = tokio::signal::ctrl_c().await;
    eprintln!("[gt] shutting down (dead-letters: {})", root.dead_letters());
    if let Some(task) = audit_task {
        task.abort();
    }
    root.shutdown();
}

/// If `GT_PG_AUDIT_URL` is set, drain every appended `EventRecord` into the Postgres audit
/// table. Returns the spawned task so the bin can abort it on shutdown. The relay is
/// idempotent (`INSERT ... ON CONFLICT DO NOTHING` on `event_id`), so a transient PG outage
/// + restart is at-least-once → exactly-once at the store.
async fn spawn_pg_audit<R>(root: &RootHandle<R>) -> Option<tokio::task::JoinHandle<()>>
where
    R: BeadRepository + Clone + 'static,
{
    let url = std::env::var("GT_PG_AUDIT_URL").ok()?;
    let audit = match PgAudit::connect(&url).await {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[gt] PG audit disabled — connect failed: {e}");
            return None;
        }
    };
    if let Err(e) = gt_store_pg::ensure_schema(audit.pool()).await {
        eprintln!("[gt] PG audit disabled — migrations failed: {e}");
        return None;
    }
    eprintln!("[gt] audit: Postgres @ {url}");
    let mut rx = root.subscribe_events();
    Some(tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(rec) => {
                    if let Err(e) = audit.append(&rec).await {
                        eprintln!("[gt] PG audit append failed ({}): {e}", rec.kind);
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    eprintln!("[gt] PG audit lagged by {n} events (catching up)");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    }))
}
