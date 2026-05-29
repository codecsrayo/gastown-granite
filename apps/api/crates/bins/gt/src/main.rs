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
//! Effects (hq-7pdl.1 / hq-mc72.12 D1): the composition root is wired with the production
//! [`RealEffects`] adapter. `sling` now spawns Rust-managed `tmux` polecats via
//! `gt_polecat::PolecatLifecycle` (no more `gt sling` subprocess — the Go binary is off the
//! runtime path); `rotate` drives the `QuotaCommand::Rotate` chain. The polecat spawn template
//! is sourced from the environment (`GT_RIG`/`GT_RIG_PATH`/`GT_POLECAT_CMD`/…).
//!
//! Graceful shutdown (paso 8.5, hq-8iur.5): SIGTERM and SIGINT trigger a deterministic
//! tear-down — root reactor first (its broadcast `Sender` drops, which is the only way the
//! outbox writer learns to stop), then the writer task is **awaited** so the final
//! `outbox_events` insert lands, then the drain task is told via an `AtomicBool` to do one
//! last pass-until-empty and exit. Only after that do we drop the telemetry guard so the
//! OTLP batch flush sees a quiescent process. No `task.abort()` on the outbox path — that
//! was the silent-data-loss seam (gate: `kill -TERM` → 0 outbox rows lost).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use gt_agent::{AgentEvent, DogKind, SessionRole};
use gt_beads::{Bead, BeadRepository, BeadStatus, InMemoryBeads};
use gt_events::Envelope;
use gt_polecat::{spawn_tmux, SpawnSpec, Tmux, TmuxCli};
use serde::Deserialize;
use gt_merge::{InMemoryMergeRepo, MergeRepository};
use gt_orchestration::{InMemoryOrchRepo, OrchRepository};
use gt_patrol::{InMemoryPatrolRepo, PatrolRepository};
use gt_plugin::PluginRegistry;
use gt_sheriff::SheriffPlugin;
use gt_root::{
    load_state, spawn_hydrated, spawn_plugin_relay, spawn_sessions_projector, MailNotifier,
    RealEffects, RootConfig, RootHandle, SystemClock,
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
    let (effects, quota_slot, polecat_supervisor) = RealEffects::from_env();

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

    // hq-mysw: route operator signals through the bead-backed mail adapter (mail = beads in
    // Gas Town). `repo.clone()` shares the same bead store the root writes to, so escalation
    // mail lands where `gt mail` reads it. Addresses are configurable; defaults match the
    // mayor→ops escalation lane.
    let mail_from = std::env::var("GT_MAIL_FROM").unwrap_or_else(|_| "mayor/".to_string());
    let mail_to = std::env::var("GT_MAIL_TO").unwrap_or_else(|_| "ops/".to_string());
    let config = RootConfig {
        notifier: Arc::new(MailNotifier::new(repo.clone(), mail_from, mail_to)),
        ..RootConfig::default()
    };

    let root = spawn_hydrated(
        repo,
        merge_repo,
        patrol_repo,
        orch_repo,
        effects,
        SystemClock,
        log_path,
        config,
        hydration,
    );
    let _ = quota_slot.set(root.quota.clone());

    let pipeline = spawn_pg_outbox_pipeline(&root).await;

    // Sessions write-path (hq-8iur.2): when Dolt is wired, mirror AgentEvent lifecycle into
    // the sessions table so the read-side (`gt-web`) owns the truth, replacing `gt sling`.
    let sessions_task = sessions_writer.map(|w| spawn_sessions_projector(&root, w));

    // Observer plugin chain. hq-evks (Paso 9.B) shipped the relay + a counter-only stub;
    // hq-92z9 (Paso 9.D) here registers the **real** `gt_sheriff::SheriffPlugin` against the
    // same trait — no wiring change other than the constructor. It forwards each observed
    // `EventRecord.kind` into the sheriff actor as a `SheriffCommand::Observe`; the actor
    // counts against its registered watches and emits `sheriff.raised` on threshold cross.
    let plugin_registry = Arc::new(
        PluginRegistry::new().register(SheriffPlugin::new(root.sheriff.clone())),
    );
    let plugin_task = spawn_plugin_relay(&root, plugin_registry);

    // hq-mc72.12 C2 — Refinery MERGE_READY live loop, under a restart+backoff supervisor.
    // First role daemon wired in production (the others — witness patrol tick, mayor orch
    // loop, deacon drain — follow). Only the orchestrator bin runs this; `gt-web` is
    // read-side and would double-submit if it also subscribed.
    let refinery_task = spawn_refinery_daemon(&root);

    // hq-mc72.12 C8 — operator warrant daemon. Watches a gt-channel for JSON-shaped warrant
    // messages and turns each into an escalation status bead. Operators emit via
    // `gt emit-event` (C7, gt-cli). Same supervised shape as the refinery.
    let warrant_task = spawn_warrant_daemon(&root);

    // hq-mc72.12 C8 — operator estop daemon. Same supervised channel shape as the warrant
    // daemon, but each message triggers a deacon drain (the SIGTERM-equivalent operator
    // signal) so an in-flight town can be brought down without a console.
    let estop_task = spawn_estop_daemon(&root);

    // hq-mc72.12 C2 Witness — periodic ticker. The reactor auto-watches workers on lease
    // expiry (hq-mc72.12.13); this loop ticks every non-escalated target so a watch past its
    // threshold actually raises an escalation without an external tick source.
    let witness_ticker = spawn_witness_ticker(&root);

    // hq-mc72.12 C5 — polecat re-supervision. Drive the PolecatSupervisor's pass on a timer:
    // a slung polecat whose tmux session died is re-slung with backoff until the work
    // completes (the reactor's Effects::release unwatches it). `RealEffects` registered each
    // polecat at sling time; this loop is the live half.
    let polecat_ticker = spawn_polecat_supervisor(polecat_supervisor);

    // hq-mc72.12 C2 Mayor — periodic orch scan. For every launched convoy not already
    // tracked, file a Pending delegation to ops so a single canonical row tracks each in-
    // flight convoy. Auto-resolve on convoy completion is a follow-up (reactor arm).
    let mayor_loop = spawn_mayor_loop(&root);

    // hq-mc72.12 C10 — dog control. Watches a gt-channel for spawn/stop of named `hq-dog-<name>`
    // sessions and tracks them as `role=dog` sessions via the agent relay. Operator-driven
    // (gt-cli `gt emit-event`); same supervised channel shape as warrant/estop.
    let dog_task = spawn_dog_daemon(&root);

    // hq-mc72.12 C10 boot-status — the orchestrator's own watchdog session. Emit Spawned for
    // `hq-boot` so the boot is auditable and shows up in `/api/sessions`, then drive periodic
    // heartbeats so liveness stays fresh. Role = `Dog(Deacon)` matches the Go `hq-boot`
    // parsing (doc 10 §Roles D4: `GTRole()=="boot"` → Deacon Name="boot").
    let _ = root
        .agent_events
        .send(Envelope::root(AgentEvent::Spawned {
            session: "hq-boot".to_string(),
            rig: "hq".to_string(),
            role: SessionRole::Dog(DogKind::Deacon),
            crew: None,
        }))
        .await;
    let boot_heartbeat = spawn_boot_heartbeat(root.agent_events.clone());

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
    // hq-mc72.12 C10 boot-status — record terminal state for the orchestrator watchdog
    // session BEFORE the drain so /api/sessions shows `hq-boot` ended cleanly even if the
    // drain hits its timeout.
    let _ = root
        .agent_events
        .send(Envelope::root(AgentEvent::SessionEnd {
            session: "hq-boot".to_string(),
        }))
        .await;
    boot_heartbeat.abort();
    let _ = boot_heartbeat.await;
    // hq-mc72.12 C2 Deacon — drain in-flight merges before tearing the reactor down. Subscribe
    // BEFORE `begin_drain` so a same-tick `DrainComplete` (pending empty at drain time) is not
    // missed. `GT_DRAIN_TIMEOUT_SECS` caps the wait (default 30) so a wedged merge cannot pin
    // shutdown forever.
    drain_via_deacon(&root).await;
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
    // Refinery is the live producer: nothing downstream consumes from it (it only feeds the
    // merge actor via `submit`, which is in-process). Abort to stop the channel watcher and
    // its subscribe thread; in-flight work for any message already submitted is durable in
    // the merge actor / audit log.
    if let Some(task) = refinery_task {
        task.abort();
        let _ = task.await;
    }
    // Warrant daemon: same teardown — abort the channel watcher; already-upserted escalation
    // beads are durable in the repo / audit log.
    if let Some(task) = warrant_task {
        task.abort();
        let _ = task.await;
    }
    // Estop daemon: abort the channel watcher; already-dispatched drains are durable in the
    // audit log (deacon.drain_requested / drain_complete are appended on exec).
    if let Some(task) = estop_task {
        task.abort();
        let _ = task.await;
    }
    // Witness ticker: a pure timer with no downstream consumer — abort it.
    witness_ticker.abort();
    let _ = witness_ticker.await;
    // Polecat supervisor ticker: same — abort the timer. Live tmux polecats keep running; the
    // bin is going down, not the town.
    polecat_ticker.abort();
    let _ = polecat_ticker.await;
    // Mayor scan loop: abort the timer; in-flight delegations stay durable in the audit log.
    mayor_loop.abort();
    let _ = mayor_loop.await;
    // Dog control channel watcher: abort it; live dog sessions keep running.
    if let Some(task) = dog_task {
        task.abort();
        let _ = task.await;
    }
    if let Some(pipeline) = pipeline {
        pipeline.shutdown_graceful().await;
    }
    eprintln!("[gt] shutdown complete");
}

/// Trigger a deacon drain and wait briefly for `DrainComplete` before the reactor is torn
/// down (hq-mc72.12 C2 Deacon). Bounded by `GT_DRAIN_TIMEOUT_SECS` (default 30s). The
/// broadcast subscribe happens *before* `begin_drain` so a same-tick `DrainComplete` (empty
/// pending set at drain time) is not missed.
async fn drain_via_deacon<R>(root: &RootHandle<R>)
where
    R: gt_beads::BeadRepository + Clone + 'static,
{
    let timeout_secs = std::env::var("GT_DRAIN_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30u64);
    let mut rx = root.subscribe_events();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Err(e) = gt_deacon::deacon::begin_drain(&root.deacon, "sigterm", now).await {
        eprintln!("[gt] drain begin failed: {e} — proceeding to shutdown");
        return;
    }
    let drained = tokio::time::timeout(Duration::from_secs(timeout_secs), async {
        loop {
            match rx.recv().await {
                Ok(rec) if rec.kind == "deacon.drain_complete" => return,
                Ok(_) => continue,
                Err(_) => return, // Closed/lagged — give up; reactor teardown follows anyway.
            }
        }
    })
    .await;
    if drained.is_err() {
        eprintln!("[gt] drain timed out after {timeout_secs}s — proceeding to shutdown");
    } else {
        eprintln!("[gt] drain complete");
    }
}

/// Spawn the supervised Refinery MERGE_READY daemon (hq-mc72.12 C2). The channel root comes
/// from `GT_CHANNEL_ROOT` (default `/gt/.channels`) and the channel name from
/// `GT_MERGE_READY_CHANNEL` (default `merge-ready`). Returns `None` when the channel can't be
/// opened — the orchestrator still boots; operators get a stderr breadcrumb. The supervisor
/// reuses [`gt_polecat::supervise_daemon`] with the default [`gt_polecat::RestartConfig`] so
/// transient channel/watcher failures auto-recover with the same backoff curve polecats use.
fn spawn_refinery_daemon<R>(root: &RootHandle<R>) -> Option<tokio::task::JoinHandle<()>>
where
    R: BeadRepository + Clone + 'static,
{
    let root_dir = std::env::var("GT_CHANNEL_ROOT")
        .unwrap_or_else(|_| "/gt/.channels".to_string());
    let channel_name =
        std::env::var("GT_MERGE_READY_CHANNEL").unwrap_or_else(|_| "merge-ready".to_string());
    let channel = match gt_channel::Channel::open(&root_dir, &channel_name) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "[gt] refinery disabled — channel open failed at {root_dir}/{channel_name}: {e}"
            );
            return None;
        }
    };
    eprintln!("[gt] refinery: MERGE_READY channel {}", channel.dir().display());

    let merge = root.merge.clone();
    Some(tokio::spawn(async move {
        let mut tracker = gt_polecat::RestartTracker::new(gt_polecat::RestartConfig::default());
        let make = move || {
            let channel = channel.clone();
            let merge = merge.clone();
            async move {
                if let Err(e) = gt_merge::refinery::run(channel, merge).await {
                    eprintln!("[gt] refinery channel error: {e} — supervisor will restart");
                }
            }
        };
        gt_polecat::supervise_daemon("refinery", make, &mut tracker, u32::MAX, now_secs)
            .await;
    }))
}

/// JSON shape an operator emits on the `warrant` channel (hq-mc72.12 C8). Sent via
/// `gt emit-event warrant '{"worker":"...","reason":"..."}'` (gt-cli C7).
#[derive(Debug, Deserialize)]
struct WarrantRequest {
    worker: String,
    reason: String,
}

/// Build the deterministic escalation bead an operator-filed warrant leaves behind. Status
/// `Failed` + priority 0 (P0) mirrors the cross-domain `escalation_bead` in root.rs (one row
/// per worker; re-filing upserts the same id instead of piling up duplicates).
fn warrant_bead(req: &WarrantRequest) -> Bead {
    Bead::new(
        format!("escalation-{}", req.worker),
        format!(
            "Warrant filed: worker {} ({})",
            req.worker, req.reason
        ),
        BeadStatus::Failed,
        0,
    )
}

/// Spawn the supervised operator-warrant daemon (hq-mc72.12 C8). Watches `GT_WARRANT_CHANNEL`
/// (default `warrant`) under `GT_CHANNEL_ROOT`; each well-formed `WarrantRequest` becomes a
/// `BeadStatus::Failed` escalation row in the bead repo. Malformed messages are acked +
/// logged (same policy as the refinery — never re-deliver). Returns `None` when the channel
/// cannot be opened. Same supervised shape as the refinery so transient channel/watcher
/// failures auto-recover with backoff.
fn spawn_warrant_daemon<R>(root: &RootHandle<R>) -> Option<tokio::task::JoinHandle<()>>
where
    R: BeadRepository + Clone + 'static,
{
    let root_dir = std::env::var("GT_CHANNEL_ROOT")
        .unwrap_or_else(|_| "/gt/.channels".to_string());
    let channel_name =
        std::env::var("GT_WARRANT_CHANNEL").unwrap_or_else(|_| "warrant".to_string());
    let channel = match gt_channel::Channel::open(&root_dir, &channel_name) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "[gt] warrant disabled — channel open failed at {root_dir}/{channel_name}: {e}"
            );
            return None;
        }
    };
    eprintln!("[gt] warrant: operator channel {}", channel.dir().display());

    let repo = root.repo.clone();
    Some(tokio::spawn(async move {
        let mut tracker = gt_polecat::RestartTracker::new(gt_polecat::RestartConfig::default());
        let make = move || {
            let channel = channel.clone();
            let repo = repo.clone();
            async move {
                let mut rx = match channel.subscribe(64) {
                    Ok(rx) => rx,
                    Err(e) => {
                        eprintln!("[gt] warrant subscribe failed: {e} — supervisor will restart");
                        return;
                    }
                };
                while let Some(msg) = rx.recv().await {
                    match serde_json::from_slice::<WarrantRequest>(&msg.payload) {
                        Ok(req) => {
                            let bead = warrant_bead(&req);
                            match repo.upsert(&bead).await {
                                Ok(()) => eprintln!(
                                    "[gt] warrant filed worker={} bead={}",
                                    req.worker, bead.id
                                ),
                                Err(e) => eprintln!(
                                    "[gt] warrant upsert failed worker={}: {e}",
                                    req.worker
                                ),
                            }
                        }
                        Err(e) => eprintln!(
                            "[gt] warrant malformed payload (ack + drop): {e}"
                        ),
                    }
                    let _ = channel.ack(&msg);
                }
            }
        };
        gt_polecat::supervise_daemon("warrant", make, &mut tracker, u32::MAX, now_secs).await;
    }))
}

/// JSON shape an operator emits on the `estop` channel (hq-mc72.12 C8). Sent via
/// `gt emit-event estop '{"by":"…","reason":"…"}'` (gt-cli C7). `by` is the operator
/// identity recorded on the resulting `deacon.drain_requested` event.
#[derive(Debug, Deserialize)]
struct EstopRequest {
    by: String,
    #[allow(dead_code)]
    reason: String,
}

/// Spawn the supervised operator-estop daemon (hq-mc72.12 C8). Same supervised channel shape
/// as the warrant daemon; each well-formed `EstopRequest` calls
/// `gt_deacon::deacon::begin_drain(by, now)` so the orchestrator drains in-flight merges
/// without a SIGTERM. Channel: `GT_CHANNEL_ROOT` + `GT_ESTOP_CHANNEL` (default `estop`).
/// Malformed messages are acked + logged (no retry).
fn spawn_estop_daemon<R>(root: &RootHandle<R>) -> Option<tokio::task::JoinHandle<()>>
where
    R: BeadRepository + Clone + 'static,
{
    let root_dir = std::env::var("GT_CHANNEL_ROOT")
        .unwrap_or_else(|_| "/gt/.channels".to_string());
    let channel_name =
        std::env::var("GT_ESTOP_CHANNEL").unwrap_or_else(|_| "estop".to_string());
    let channel = match gt_channel::Channel::open(&root_dir, &channel_name) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "[gt] estop disabled — channel open failed at {root_dir}/{channel_name}: {e}"
            );
            return None;
        }
    };
    eprintln!("[gt] estop: operator channel {}", channel.dir().display());

    let deacon = root.deacon.clone();
    Some(tokio::spawn(async move {
        let mut tracker = gt_polecat::RestartTracker::new(gt_polecat::RestartConfig::default());
        let make = move || {
            let channel = channel.clone();
            let deacon = deacon.clone();
            async move {
                let mut rx = match channel.subscribe(64) {
                    Ok(rx) => rx,
                    Err(e) => {
                        eprintln!("[gt] estop subscribe failed: {e} — supervisor will restart");
                        return;
                    }
                };
                while let Some(msg) = rx.recv().await {
                    match serde_json::from_slice::<EstopRequest>(&msg.payload) {
                        Ok(req) => {
                            match gt_deacon::deacon::begin_drain(&deacon, &req.by, now_secs())
                                .await
                            {
                                Ok(()) => eprintln!("[gt] estop accepted by={}", req.by),
                                Err(e) => eprintln!(
                                    "[gt] estop drain failed by={}: {e}",
                                    req.by
                                ),
                            }
                        }
                        Err(e) => eprintln!(
                            "[gt] estop malformed payload (ack + drop): {e}"
                        ),
                    }
                    let _ = channel.ack(&msg);
                }
            }
        };
        gt_polecat::supervise_daemon("estop", make, &mut tracker, u32::MAX, now_secs).await;
    }))
}

/// JSON shape an operator emits on the `dog` channel (hq-mc72.12 C10). `action` is `spawn`
/// or `stop`; `name` keys the `hq-dog-<name>` town session.
#[derive(Debug, Deserialize)]
struct DogRequest {
    name: String,
    action: String,
}

/// Spawn the supervised dog-control daemon (hq-mc72.12 C10). Watches `GT_DOG_CHANNEL`
/// (default `dog`) under `GT_CHANNEL_ROOT`. `spawn` creates a detached `hq-dog-<name>` tmux
/// session (a town-level supervisory agent, `GT_ROLE=dog`) and tracks it via the agent relay
/// as a `role=dog` session; `stop` kills the session and emits `SessionEnd`. Operator-driven
/// (gt-cli `gt emit-event`); malformed / unknown-action messages are acked + logged.
fn spawn_dog_daemon<R>(root: &RootHandle<R>) -> Option<tokio::task::JoinHandle<()>>
where
    R: BeadRepository + Clone + 'static,
{
    let root_dir =
        std::env::var("GT_CHANNEL_ROOT").unwrap_or_else(|_| "/gt/.channels".to_string());
    let channel_name = std::env::var("GT_DOG_CHANNEL").unwrap_or_else(|_| "dog".to_string());
    let channel = match gt_channel::Channel::open(&root_dir, &channel_name) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "[gt] dog control disabled — channel open failed at {root_dir}/{channel_name}: {e}"
            );
            return None;
        }
    };
    eprintln!("[gt] dog control: operator channel {}", channel.dir().display());

    let workdir = std::env::var("GT_RIG_PATH").unwrap_or_else(|_| "/gt".to_string());
    let command = std::env::var("GT_POLECAT_CMD").unwrap_or_else(|_| "claude".to_string());
    let agent_events = root.agent_events.clone();
    Some(tokio::spawn(async move {
        let mut tracker = gt_polecat::RestartTracker::new(gt_polecat::RestartConfig::default());
        let make = move || {
            let channel = channel.clone();
            let agent_events = agent_events.clone();
            let workdir = workdir.clone();
            let command = command.clone();
            async move {
                let tmux = TmuxCli::new();
                let mut rx = match channel.subscribe(64) {
                    Ok(rx) => rx,
                    Err(e) => {
                        eprintln!("[gt] dog subscribe failed: {e} — supervisor will restart");
                        return;
                    }
                };
                while let Some(msg) = rx.recv().await {
                    match serde_json::from_slice::<DogRequest>(&msg.payload) {
                        Ok(req) => {
                            let session = format!("hq-dog-{}", req.name);
                            match req.action.as_str() {
                                "spawn" => {
                                    let spec = SpawnSpec {
                                        session: session.clone(),
                                        rig: "hq".to_string(),
                                        polecat: req.name.clone(),
                                        crew: None,
                                        workdir: std::path::PathBuf::from(&workdir),
                                        command: command.clone(),
                                        args: Vec::new(),
                                        env: vec![
                                            ("GT_ROLE".to_string(), "dog".to_string()),
                                            ("GT_DOG_NAME".to_string(), req.name.clone()),
                                        ],
                                        hook_bead: None,
                                        issue: None,
                                        heartbeat: std::env::temp_dir()
                                            .join(format!("{session}.heartbeat")),
                                    };
                                    match spawn_tmux(&tmux, &spec) {
                                        Ok(()) => {
                                            let _ = agent_events
                                                .send(Envelope::root(AgentEvent::Spawned {
                                                    session: session.clone(),
                                                    rig: "hq".to_string(),
                                                    role: SessionRole::Dog(DogKind::Dog),
                                                    crew: None,
                                                }))
                                                .await;
                                            eprintln!("[gt] dog spawned session={session}");
                                        }
                                        Err(e) => eprintln!(
                                            "[gt] dog spawn failed session={session}: {e}"
                                        ),
                                    }
                                }
                                "stop" => {
                                    let _ = tmux.kill_session(&session);
                                    let _ = agent_events
                                        .send(Envelope::root(AgentEvent::SessionEnd {
                                            session: session.clone(),
                                        }))
                                        .await;
                                    eprintln!("[gt] dog stopped session={session}");
                                }
                                other => eprintln!(
                                    "[gt] dog unknown action={other} (ack + drop)"
                                ),
                            }
                        }
                        Err(e) => {
                            eprintln!("[gt] dog malformed payload (ack + drop): {e}")
                        }
                    }
                    let _ = channel.ack(&msg);
                }
            }
        };
        gt_polecat::supervise_daemon("dog", make, &mut tracker, u32::MAX, now_secs).await;
    }))
}

/// Spawn the periodic Mayor orchestration loop (hq-mc72.12 C2 Mayor). Every
/// `GT_MAYOR_TICK_SECS` (default 60, floored at 1) it snapshots the orch convoy board + the
/// mayor delegations, and for each `ConvoyState::Launched` convoy not already tracked it
/// files a Pending delegation `convoy-<id>` to `ops`. Idempotent — re-scans skip already-
/// delegated convoys. The auto-resolve on convoy completion (a reactor arm) is a follow-up.
fn spawn_mayor_loop<R>(root: &RootHandle<R>) -> tokio::task::JoinHandle<()>
where
    R: BeadRepository + Clone + 'static,
{
    let mayor = root.mayor.clone();
    let orch = root.orch.clone();
    let interval_secs = std::env::var("GT_MAYOR_TICK_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(60)
        .max(1);
    eprintln!("[gt] mayor loop: orch scan every {interval_secs}s");
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(interval_secs));
        tick.tick().await; // skip the immediate first tick
        loop {
            tick.tick().await;
            let convoys = orch.snapshot().await;
            let mayor_state = mayor.snapshot().await;
            for convoy in convoys {
                if !matches!(convoy.state, gt_orchestration::ConvoyState::Launched) {
                    continue;
                }
                let id = format!("convoy-{}", convoy.id);
                if mayor_state.delegations.contains_key(&id) {
                    continue;
                }
                let summary = format!(
                    "convoy {} launched ({} members)",
                    convoy.id,
                    convoy.members.len()
                );
                if let Err(e) = gt_mayor::mayor::delegate(&mayor, id, "ops", summary).await {
                    eprintln!("[gt] mayor delegate failed: {e}");
                }
            }
        }
    })
}

/// Periodic Heartbeat for the `hq-boot` watchdog session (hq-mc72.12 C10 boot-status). Every
/// `GT_BOOT_HEARTBEAT_SECS` (default 30, floored at 1) it pushes an `AgentEvent::Heartbeat`
/// through the agent relay so `/api/sessions` shows the orchestrator as live. Aborted on
/// shutdown.
fn spawn_boot_heartbeat(
    agent_events: tokio::sync::mpsc::Sender<Envelope<AgentEvent>>,
) -> tokio::task::JoinHandle<()> {
    let interval_secs = std::env::var("GT_BOOT_HEARTBEAT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(30)
        .max(1);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(interval_secs));
        tick.tick().await; // skip the immediate first tick (Spawned was just emitted)
        loop {
            tick.tick().await;
            let _ = agent_events
                .send(Envelope::root(AgentEvent::Heartbeat {
                    session: "hq-boot".to_string(),
                }))
                .await;
        }
    })
}

/// Spawn the periodic witness ticker (hq-mc72.12 C2 Witness). Every `GT_WITNESS_TICK_SECS`
/// (default 30, floored at 1) it snapshots the witness state and ticks each non-escalated
/// target; a tick past the target's threshold raises `WitnessEvent::EscalationRaised`, which
/// the reactor's hq-mysw arm turns into a status bead + notification. A pure timer (no
/// channel), so it never returns — the bin aborts it on shutdown. Errors are logged and the
/// loop continues so one failed tick never stops the watchdog.
fn spawn_witness_ticker<R>(root: &RootHandle<R>) -> tokio::task::JoinHandle<()>
where
    R: BeadRepository + Clone + 'static,
{
    let witness = root.witness.clone();
    let interval_secs = std::env::var("GT_WITNESS_TICK_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(30)
        .max(1);
    eprintln!("[gt] witness: ticking watched targets every {interval_secs}s");
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(interval_secs));
        // The first `interval` tick fires immediately; skip it so we never tick an empty set
        // before any worker is watched.
        tick.tick().await;
        loop {
            tick.tick().await;
            let state = witness.snapshot().await;
            let now = now_secs();
            for target in state.targets.values() {
                if target.escalated {
                    continue;
                }
                if let Err(e) = gt_witness::witness::tick(&witness, &target.worker, now).await {
                    eprintln!("[gt] witness tick failed worker={}: {e}", target.worker);
                }
            }
        }
    })
}

/// Drive the [`gt_polecat::PolecatSupervisor`] supervision pass on a timer (hq-mc72.12 C5).
/// Every `GT_POLECAT_TICK_SECS` (default 15, floored at 1) it re-slings any watched polecat
/// whose tmux session has died (subject to backoff + the restart cap). `tick` is synchronous
/// `tmux` I/O, so it runs on the blocking pool to keep the runtime workers free. Aborted on
/// shutdown.
fn spawn_polecat_supervisor(
    supervisor: std::sync::Arc<gt_polecat::PolecatSupervisor>,
) -> tokio::task::JoinHandle<()> {
    let interval_secs = std::env::var("GT_POLECAT_TICK_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(15)
        .max(1);
    eprintln!("[gt] polecat supervisor: re-sling check every {interval_secs}s");
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(interval_secs));
        tick.tick().await; // skip the immediate first fire
        loop {
            tick.tick().await;
            let sup = supervisor.clone();
            // `tick` is blocking tmux I/O; keep it off the runtime workers.
            let reslung = tokio::task::spawn_blocking(move || sup.tick(now_secs()))
                .await
                .unwrap_or(0);
            if reslung > 0 {
                eprintln!("[gt] polecat supervisor re-slung {reslung} dead polecat(s)");
            }
        }
    })
}

/// Edge-stamped unix seconds for the supervisor clock.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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
