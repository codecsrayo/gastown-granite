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
//!
//! Graceful shutdown (paso 8.5, hq-8iur.5): SIGTERM and SIGINT trigger a deterministic
//! tear-down — root reactor first (its broadcast `Sender` drops, which is the only way the
//! outbox writer learns to stop), then the writer task is **awaited** so the final
//! `outbox_events` insert lands, then the drain task is told via an `AtomicBool` to do one
//! last pass-until-empty and exit. Only after that do we drop the telemetry guard so the
//! OTLP batch flush sees a quiescent process. No `task.abort()` on the outbox path — that
//! was the silent-data-loss seam (gate: `kill -TERM` → 0 outbox rows lost).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use gt_beads::{BeadRepository, InMemoryBeads};
use gt_merge::{InMemoryMergeRepo, MergeRepository};
use gt_orchestration::{InMemoryOrchRepo, OrchRepository};
use gt_patrol::{InMemoryPatrolRepo, PatrolRepository};
use gt_plugin::{PluginRegistry, SheriffPlugin};
use gt_root::{
    load_state, spawn_hydrated, spawn_plugin_relay, spawn_sessions_projector, RealEffects,
    RootConfig, RootHandle, SystemClock,
};
use gt_store_dolt::{DoltBeads, DoltMerge, DoltOrch, DoltPatrol, DoltSessions};
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
                let dolt_merge = DoltMerge::connect(&url).expect("connect Dolt merge");
                dolt_merge
                    .ensure_schema()
                    .await
                    .expect("Dolt merge ensure_schema");
                let dolt_patrol = DoltPatrol::connect(&url).expect("connect Dolt patrol");
                dolt_patrol
                    .ensure_schema()
                    .await
                    .expect("Dolt patrol ensure_schema");
                let dolt_orch = DoltOrch::connect(&url).expect("connect Dolt orch");
                dolt_orch
                    .ensure_schema()
                    .await
                    .expect("Dolt orch ensure_schema");
                // Sessions write-path target (hq-8iur.2): the projector mirrors AgentEvent
                // lifecycle into this canonical table — the same one the read-side serves.
                let dolt_sessions = DoltSessions::connect(&url).expect("connect Dolt sessions");
                dolt_sessions
                    .ensure_schema()
                    .await
                    .expect("Dolt sessions ensure_schema");
                eprintln!("[gt] beads + sessions + merge/patrol/orch: Dolt @ {url}");
                run(
                    Arc::new(dolt),
                    Arc::new(dolt_merge),
                    Arc::new(dolt_patrol),
                    Arc::new(dolt_orch),
                    Some(Arc::new(dolt_sessions)),
                    &log_path,
                    gt_bin,
                )
                .await;
            }
            None => {
                eprintln!(
                    "[gt] domain state: in-memory (set GT_DOLT_URL for Dolt persistence)"
                );
                run(
                    Arc::new(InMemoryBeads::default()),
                    Arc::new(InMemoryMergeRepo::default()),
                    Arc::new(InMemoryPatrolRepo::default()),
                    Arc::new(InMemoryOrchRepo::default()),
                    // No sessions read-side in headless in-memory mode — skip the projector.
                    None,
                    &log_path,
                    gt_bin,
                )
                .await;
            }
        }
    });
}

async fn run<R, MR, PR, OR>(
    repo: Arc<R>,
    merge_repo: Arc<MR>,
    patrol_repo: Arc<PR>,
    orch_repo: Arc<OR>,
    sessions_writer: Option<Arc<DoltSessions>>,
    log_path: &str,
    gt_bin: PathBuf,
) where
    R: BeadRepository + 'static,
    Arc<R>: BeadRepository + Clone + 'static,
    MR: MergeRepository + 'static,
    Arc<MR>: MergeRepository + 'static,
    PR: PatrolRepository + 'static,
    Arc<PR>: PatrolRepository + 'static,
    OR: OrchRepository + 'static,
    Arc<OR>: OrchRepository + 'static,
{
    let (effects, quota_slot) = RealEffects::new(gt_bin);

    // Boot hydration (hq-8iur.1): fold the audit log into per-domain state before spawning,
    // so a restart restores in-flight merge slots / patrol leases / convoy progress / quota
    // status without re-emitting events. A missing log file is treated as a fresh install.
    let hydration = match load_state(log_path) {
        Ok(h) => {
            eprintln!(
                "[gt] hydrated from {} records: {} merge slots, {} patrol leases, {} convoys, {} accounts",
                h.records_folded,
                h.state.merge.board.len(),
                h.state.patrol.tracker.len(),
                h.state.orch.convoys.len(),
                h.state.quota.accounts.len(),
            );
            h
        }
        Err(e) => {
            eprintln!("[gt] hydration skipped — log load failed: {e}");
            Default::default()
        }
    };

    let root = spawn_hydrated(
        repo,
        merge_repo,
        patrol_repo,
        orch_repo,
        effects,
        SystemClock,
        log_path,
        RootConfig::default(),
        hydration,
    );
    let _ = quota_slot.set(root.quota.clone());

    let pipeline = spawn_pg_outbox_pipeline(&root).await;

    // Sessions write-path (hq-8iur.2): when Dolt is wired, mirror AgentEvent lifecycle into
    // the sessions table so the read-side (`gt-web`) owns the truth, replacing `gt sling`.
    let sessions_task = sessions_writer.map(|w| spawn_sessions_projector(&root, w));

    // Observer plugin chain (hq-evks): Sheriff is registered as a stub so the relay has a
    // live consumer end-to-end; 9.D (hq-92z9) replaces it with the real watchdog through the
    // same trait, no wiring change here.
    let plugin_registry = Arc::new(PluginRegistry::new().register(SheriffPlugin::new()));
    let plugin_task = spawn_plugin_relay(&root, plugin_registry);

    eprintln!(
        "[gt] composition root up — event log: {}",
        root.log_path().display()
    );
    eprintln!("[gt] (edges: scheduler/patrol/merge/quota/orchestration actors live; drive via handles)");

    wait_for_signal("gt").await;

    eprintln!(
        "[gt] shutting down (dead-letters: {})",
        root.dead_letters()
    );
    // Stop the reactor first — that drops its broadcast Sender, which is the only way the
    // outbox writer (and the sessions projector) receive `Closed`. Once root.shutdown returns,
    // no new EventRecords are produced and the consumers drain to completion deterministically.
    root.shutdown();
    // The sessions projector exits naturally on broadcast `Closed` — await it instead of
    // aborting, matching the no-data-loss discipline of the outbox pipeline (hq-8iur.5).
    if let Some(task) = sessions_task {
        let _ = task.await;
    }
    // The plugin relay also exits on broadcast `Closed`; await so any in-flight `on_event`
    // returns before tearing telemetry down.
    let _ = plugin_task.await;
    if let Some(pipeline) = pipeline {
        pipeline.shutdown_graceful().await;
    }
    eprintln!("[gt] shutdown complete");
}

/// Wait for SIGTERM or SIGINT. Returns the unit when the first arrives. If signal install
/// fails (non-Unix), the future never resolves and the process keeps running until killed
/// externally — better than auto-exiting on startup.
async fn wait_for_signal(name: &'static str) {
    use tokio::signal::unix::{signal, SignalKind};
    let term = signal(SignalKind::terminate());
    let int = signal(SignalKind::interrupt());
    match (term, int) {
        (Ok(mut term), Ok(mut int)) => {
            tokio::select! {
                _ = term.recv() => eprintln!("[{name}] SIGTERM received — draining"),
                _ = int.recv() => eprintln!("[{name}] SIGINT received — draining"),
            }
        }
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("[{name}] signal install failed: {e}; running until killed externally");
            std::future::pending::<()>().await;
        }
    }
}

/// Outbox pipeline handles. Returned by [`spawn_pg_outbox_pipeline`] so the bin can take it
/// through the graceful-shutdown protocol instead of `task.abort()`-ing on TERM (which is the
/// behavior the gate test for paso 8.5 explicitly forbids: zero outbox rows lost).
struct OutboxPipeline {
    /// Reads `EventRecord`s off the root's broadcast and commits them to `outbox_events`
    /// (single-TX with `token_usage` when applicable). Exits naturally on broadcast `Closed`.
    writer: tokio::task::JoinHandle<()>,
    /// Fans drained rows into `audit_events` + `feed_projections`. Watches `drain_shutdown`
    /// to switch from the periodic-tick loop to a final pass-until-empty before exiting.
    drain: tokio::task::JoinHandle<()>,
    /// Cooperative shutdown flag for the drain task. Set by `shutdown_graceful` after the
    /// writer has finished, so the drain knows the upstream is closed and can stop polling.
    drain_shutdown: Arc<AtomicBool>,
}

impl OutboxPipeline {
    /// Shut down in dependency order:
    /// 1. `await` the writer — it returns once it has consumed every `EventRecord` already in
    ///    the broadcast (the reactor having dropped its Sender via `root.shutdown()`).
    /// 2. Signal the drain that no more rows are coming and `await` it — it does a final
    ///    pass-until-empty, marks every pending row drained, then exits.
    async fn shutdown_graceful(self) {
        // The writer drains naturally — the broadcast Sender has already been dropped by
        // `root.shutdown()`, so its `rx.recv()` returns `Closed` once the ring is empty.
        if let Err(e) = self.writer.await {
            eprintln!("[gt] outbox writer join error: {e}");
        }
        // Tell the drain to stop the periodic tick and finish what is left in `outbox_events`.
        self.drain_shutdown.store(true, Ordering::SeqCst);
        if let Err(e) = self.drain.await {
            eprintln!("[gt] outbox drain join error: {e}");
        }
    }
}

/// If `GT_PG_AUDIT_URL` is set, wire the doc-04 §3 outbox pipeline:
///   1. A broadcast subscriber publishes every `EventRecord` into `outbox_events` (and,
///      for `quota.tokens_sampled`, the matching `token_usage` row in the same TX).
///   2. A periodic drain moves pending outbox rows into `audit_events` +
///      `feed_projections`, marking them drained only after both downstream writes
///      succeed.
///
/// Both halves are idempotent on `event_id`; a crash between (1) and (2) is recovered on
/// the next drain tick. Returns an [`OutboxPipeline`] whose `shutdown_graceful` drains both
/// tasks in dependency order instead of aborting them.
async fn spawn_pg_outbox_pipeline<R>(root: &RootHandle<R>) -> Option<OutboxPipeline>
where
    R: BeadRepository + Clone + 'static,
{
    let url = std::env::var("GT_PG_AUDIT_URL").ok()?;
    let writer = match PgOutboxWriter::connect(&url).await {
        Ok(w) => w,
        Err(e) => {
            eprintln!("[gt] PG outbox disabled — connect failed: {e}");
            return None;
        }
    };
    if let Err(e) = gt_store_pg::ensure_schema(writer.pool()).await {
        eprintln!("[gt] PG outbox disabled — migrations failed: {e}");
        return None;
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

    let drain_shutdown = Arc::new(AtomicBool::new(false));
    let drain_shutdown_for_task = drain_shutdown.clone();
    let drain_task = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(200));
        loop {
            tick.tick().await;
            if drain_shutdown_for_task.load(Ordering::SeqCst) {
                // Final pass-until-empty. Bound the loop so a broken connection cannot pin
                // the shutdown — if the drain errors, log and exit.
                loop {
                    match drain.drain_batch(256).await {
                        Ok(0) => break,
                        Ok(_) => continue,
                        Err(e) => {
                            eprintln!("[gt] PG outbox final drain failed: {e}");
                            break;
                        }
                    }
                }
                break;
            }
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

    Some(OutboxPipeline {
        writer: writer_task,
        drain: drain_task,
        drain_shutdown,
    })
}
