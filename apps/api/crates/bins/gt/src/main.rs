//! `gt` binary — the thin boot. Per `docs/01-architecture.md`, the **single** tokio runtime
//! is created here, in `bins/`; the domain crates never create one, they receive handles.
//!
//! Persistence (hq-j9ou): when `GT_DOLT_URL` is set, the bead repo is the real Dolt-backed
//! `gt-store-dolt::DoltBeads`; otherwise the bin falls back to the in-memory port so host
//! runs without a Dolt server still work end-to-end. When `GT_PG_AUDIT_URL` is set the boot
//! wires the Postgres outbox pipeline (hq-7owq, canonical EventStore + read-side feed
//! projection): a broadcast subscriber commits each `EventRecord` into `outbox_events`
//! (and, for `quota.tokens_sampled`, the matching `token_usage` row in the same
//! transaction — doc-04 §3), and a drain task fans the outbox into `audit_events` +
//! `feed_projections`. The legacy direct audit relay is gone: `audit_events` is now written
//! by the drain so a crash between broadcast and audit is recovered from the durable outbox
//! instead of lost. The local `.events.jsonl` keeps writing in both modes as a spill.
//!
//! Effects (hq-7pdl.1): the composition root is wired with the production [`RealEffects`]
//! adapter (`gt sling` child processes + the `QuotaCommand::Rotate` chain). The `gt` binary
//! path is configurable via `GT_BIN`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gt_beads::{BeadRepository, InMemoryBeads};
use gt_root::{spawn, RealEffects, RootConfig, RootHandle, SystemClock};
use gt_store_dolt::DoltBeads;
use gt_store_pg::{PgOutboxDrain, PgOutboxWriter};
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

    let pg_tasks = spawn_pg_outbox_pipeline(&root).await;

    eprintln!(
        "[gt] composition root up — event log: {}",
        root.log_path().display()
    );
    eprintln!("[gt] (edges: scheduler/patrol/merge/quota/orchestration actors live; drive via handles)");

    let _ = tokio::signal::ctrl_c().await;
    eprintln!("[gt] shutting down (dead-letters: {})", root.dead_letters());
    for task in pg_tasks {
        task.abort();
    }
    root.shutdown();
}

/// If `GT_PG_AUDIT_URL` is set, wire the doc-04 §3 outbox pipeline:
///   1. A broadcast subscriber publishes every `EventRecord` into `outbox_events` (and,
///      for `quota.tokens_sampled`, the matching `token_usage` row in the same TX).
///   2. A periodic drain moves pending outbox rows into `audit_events` +
///      `feed_projections`, marking them drained only after both downstream writes
///      succeed.
///
/// Both halves are idempotent on `event_id`; a crash between (1) and (2) is recovered on
/// the next drain tick. Returns the spawned tasks so the bin can abort them on shutdown.
async fn spawn_pg_outbox_pipeline<R>(root: &RootHandle<R>) -> Vec<tokio::task::JoinHandle<()>>
where
    R: BeadRepository + Clone + 'static,
{
    let Some(url) = std::env::var("GT_PG_AUDIT_URL").ok() else {
        return Vec::new();
    };
    let writer = match PgOutboxWriter::connect(&url).await {
        Ok(w) => w,
        Err(e) => {
            eprintln!("[gt] PG outbox disabled — connect failed: {e}");
            return Vec::new();
        }
    };
    if let Err(e) = gt_store_pg::ensure_schema(writer.pool()).await {
        eprintln!("[gt] PG outbox disabled — migrations failed: {e}");
        return Vec::new();
    }
    // Reuse the writer's pool for the drain so we keep a single bounded connection budget.
    let drain = PgOutboxDrain::new(writer.pool().clone());
    eprintln!("[gt] outbox: Postgres @ {url} (writer + drain)");

    let mut rx = root.subscribe_events();
    let writer_task = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(rec) => {
                    if let Err(e) = writer.publish(&rec).await {
                        eprintln!("[gt] PG outbox publish failed ({}): {e}", rec.kind);
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    eprintln!("[gt] PG outbox lagged by {n} events (catching up)");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    let drain_task = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(200));
        loop {
            tick.tick().await;
            match drain.drain_batch(64).await {
                Ok(0) => {}
                Ok(n) => {
                    if n == 64 {
                        // Hot — re-arm immediately to drain the rest.
                        tick.reset_immediately();
                    }
                }
                Err(e) => eprintln!("[gt] PG outbox drain failed: {e}"),
            }
        }
    });

    vec![writer_task, drain_task]
}
