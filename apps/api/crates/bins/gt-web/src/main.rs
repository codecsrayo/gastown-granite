//! `gt-web` binary — the read-side process. Boots the composition root (`bins/gt`) plus the
//! Axum router; the single tokio runtime lives here per `docs/01-architecture.md`.
//!
//! Persistence (hq-j9ou): when `GT_DOLT_URL` is set, the bead repo behind `AppState.beads`
//! and behind the root is the real Dolt-backed `DoltBeads`; otherwise the bin keeps the
//! in-memory port so the API stays runnable on a host without Dolt. `GT_PG_AUDIT_URL`
//! mirrors every appended `EventRecord` into Postgres (canonical audit per docs/04).
//!
//! Effects (hq-7pdl.1 / hq-mc72.12 D1): wired with the production [`RealEffects`] adapter.
//! `sling` spawns Rust-managed `tmux` polecats via `gt_polecat::PolecatLifecycle` (the `gt
//! sling` subprocess is gone — the Go binary is off the runtime path); `rotate` drives the
//! `QuotaCommand::Rotate` chain. The polecat spawn template is sourced from the environment.
//!
//! IAM (hq-7pdl.2): bearer-token middleware sits in front of every `/api` route. The secret
//! is read from `GT_WEB_TOKEN`; without it the bin refuses to start unless
//! `GT_WEB_AUTH=disabled` is set explicitly (intended for in-tree dev only). Every accepted
//! / rejected request lands in the shared `events.jsonl` as a `web.*` frontier-audit record.
//!
//! Operational readiness (paso 8.5, hq-8iur.5):
//! - `/health` (liveness) and `/readyz` (readiness) sit **outside** the IAM middleware so
//!   systemd / kube probes work without carrying the bearer token. `/metrics` also moved out
//!   of the auth layer — Prometheus scrapes anonymously.
//! - The bin captures `Arc<DoltBeads>` and `Arc<PgAudit>` clones at boot and registers them
//!   as readyz probes (`SELECT 1`), so a flapping backend visibly fails the probe instead of
//!   tarpitting the IAM-protected routes.
//! - The boot defaults `hydration_done=true` so the API is reachable from byte 0 until
//!   paso 8.1 (boot replay, hq-8iur.1) lands; once it does, the hydrating task uses
//!   [`ReadinessGate::pending_hydration`] + [`HydrationHandle::set`] to gate readyz on a
//!   real warm-up.
//! - SIGTERM / SIGINT trigger axum's graceful shutdown (in-flight requests drain), then the
//!   root reactor is stopped which closes the broadcast hub, the PG audit task drains the
//!   final batch and exits, and the telemetry guard flushes the OTLP batch on drop. No
//!   `task.abort()` on the hot drain path — that was the silent-data-loss seam.

use std::sync::Arc;

use gt_agent::{InMemorySessions, SessionQueries, SessionWriter};
use gt_audit::JsonlWriter;
use gt_beads::{BeadRepository, InMemoryBeads};
use gt_merge::{InMemoryMergeRepo, MergeRepository};
use gt_orchestration::{InMemoryOrchRepo, OrchRepository};
use gt_patrol::{InMemoryPatrolRepo, PatrolRepository};
use gt_root::{
    load_state, spawn_hydrated, spawn_sessions_projector, RealEffects, RootConfig, RootHandle,
    SystemClock,
};
use gt_store_dolt::{DoltBeads, DoltIssues, DoltMerge, DoltOrch, DoltPatrol, DoltSessions};
use gt_store_pg::PgAudit;
use gt_telemetry::{init as init_telemetry, TelemetryConfig};
use gt_web::{
    AppState, AuthConfig, DoltIssueCommenter, IssueCommenter, JsonlWebAudit,
    LifecyclePolecatRespawner, PolecatControl, PolecatRespawner, ReadinessGate,
    ReadinessGateBuilder, TmuxPolecatControl, WebAuditSink,
};


fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    let _telemetry = init_telemetry(TelemetryConfig::from_env("gt-web"))
        .map_err(|e| eprintln!("[gt-web] telemetry init: {e} (continuing without exporter)"))
        .ok();

    runtime.block_on(async {
        let log_path = std::env::var("GT_EVENT_LOG")
            .unwrap_or_else(|_| "/tmp/gt.events.jsonl".to_string());
        let bind = std::env::var("GT_WEB_BIND").unwrap_or_else(|_| "127.0.0.1:8787".to_string());

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

        // PG audit is opt-in; if configured, we both spawn the audit relay AND register a
        // readyz probe against the same handle. Order matters: connect first so we can
        // register the probe before the gate is built.
        let pg_audit = connect_pg_audit().await;

        // Seed the gate with the PG probe (if any). The Dolt probe is added inside the
        // Dolt-on branch where the concrete handle exists.
        let mut gate_builder = ReadinessGateBuilder::new();
        if let Some(a) = &pg_audit {
            let pinger = a.clone();
            gate_builder = gate_builder.with_probe("pg-audit", move || {
                let p = pinger.clone();
                async move { p.ping().await.map_err(|e| e.to_string()) }
            });
        }

        match std::env::var("GT_DOLT_URL").ok() {
            Some(url) => {
                let dolt = Arc::new(DoltBeads::connect(&url).expect("connect Dolt"));
                dolt.ensure_schema().await.expect("Dolt ensure_schema");
                let dolt_sessions = DoltSessions::connect(&url).expect("connect Dolt sessions");
                dolt_sessions
                    .ensure_schema()
                    .await
                    .expect("Dolt sessions ensure_schema");
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
                // `hq.issues` reader (hq-fe-api-r.9) — distinct from `beads` (5-col
                // dispatcher table). ensure_schema is idempotent: backfills the taxonomy
                // columns the `hq-taxon` family added without re-running on warm tables.
                let dolt_issues = DoltIssues::connect(&url).expect("connect Dolt issues");
                dolt_issues
                    .ensure_schema()
                    .await
                    .expect("Dolt issues ensure_schema");
                eprintln!(
                    "[gt-web] beads + sessions + merge/patrol/orch + issues: Dolt @ {url}"
                );

                let dolt_probe = dolt.clone();
                let gate = gate_builder
                    .with_probe("dolt", move || {
                        let d = dolt_probe.clone();
                        async move { d.ping().await.map_err(|e| e.to_string()) }
                    })
                    .build();

                serve(
                    dolt,
                    Arc::new(dolt_sessions),
                    Arc::new(dolt_merge),
                    Arc::new(dolt_patrol),
                    Arc::new(dolt_orch),
                    Some(Arc::new(dolt_issues)),
                    &log_path,
                    &bind,
                    auth,
                    gate,
                    pg_audit,
                )
                .await;
            }
            None => {
                eprintln!(
                    "[gt-web] domain state + sessions: in-memory (set GT_DOLT_URL for Dolt persistence)"
                );
                let gate = gate_builder.build();
                serve(
                    Arc::new(InMemoryBeads::default()),
                    Arc::new(InMemorySessions::default()),
                    Arc::new(InMemoryMergeRepo::default()),
                    Arc::new(InMemoryPatrolRepo::default()),
                    Arc::new(InMemoryOrchRepo::default()),
                    None,
                    &log_path,
                    &bind,
                    auth,
                    gate,
                    pg_audit,
                )
                .await;
            }
        }
    });
}

async fn serve<R, SQ, MR, PR, OR>(
    beads: Arc<R>,
    sessions: Arc<SQ>,
    merge_repo: Arc<MR>,
    patrol_repo: Arc<PR>,
    orch_repo: Arc<OR>,
    issues: Option<Arc<DoltIssues>>,
    log_path: &str,
    bind: &str,
    auth: AuthConfig,
    gate: ReadinessGate,
    pg_audit: Option<Arc<PgAudit>>,
) where
    R: BeadRepository + Send + Sync + 'static,
    Arc<R>: BeadRepository + Clone + 'static,
    SQ: SessionQueries + SessionWriter + Send + Sync + 'static,
    MR: MergeRepository + 'static,
    Arc<MR>: MergeRepository + 'static,
    PR: PatrolRepository + 'static,
    Arc<PR>: PatrolRepository + 'static,
    OR: OrchRepository + 'static,
    Arc<OR>: OrchRepository + 'static,
{
    // gt-web is the read-side: it wires RealEffects for parity but does not drive the polecat
    // supervision pass (the orchestrator `gt` bin owns that timer), so the supervisor handle
    // is dropped here.
    let (effects, quota_slot, _polecat_supervisor) = RealEffects::from_env();

    // Boot hydration (hq-8iur.1) — match the gt bin so a gt-web restart restores the same
    // in-flight state from the shared event log before serving the read-side.
    let hydration = match load_state(log_path) {
        Ok(h) => {
            eprintln!(
                "[gt-web] hydrated from {} records: {} merge slots, {} patrol leases, {} convoys, {} accounts",
                h.records_folded,
                h.state.merge.board.len(),
                h.state.patrol.tracker.len(),
                h.state.orch.convoys.len(),
                h.state.quota.accounts.len(),
            );
            h
        }
        Err(e) => {
            eprintln!("[gt-web] hydration skipped — log load failed: {e}");
            Default::default()
        }
    };

    let root = spawn_hydrated(
        beads.clone(),
        merge_repo.clone(),
        patrol_repo,
        orch_repo,
        effects,
        SystemClock,
        log_path,
        RootConfig::default(),
        hydration,
    );
    let _ = quota_slot.set(root.quota.clone());

    // Sessions write-path (hq-8iur.2): mirror AgentEvent lifecycle into the same sessions
    // store the read-side serves. Idempotent upserts make running it here (and in the `gt`
    // bin) safe — at-least-once delivery, REPLACE/UPDATE by id.
    let sessions_projector = spawn_sessions_projector(&root, sessions.clone());

    let audit_task = pg_audit.as_ref().map(|a| {
        let rx = root.subscribe_events();
        spawn_pg_audit_task(a.clone(), rx)
    });

    // Frontier audit writes to the same events.jsonl the reactor appends to — the boundary's
    // who-consulted-what records share the system log (`web.*` frontier-audit prefix).
    let writer: Arc<dyn gt_audit::EventStore + Send + Sync> =
        Arc::new(JsonlWriter::new(root.log_path()));
    let audit: Arc<dyn WebAuditSink> = Arc::new(JsonlWebAudit::new(writer));

    // Town root for `GET /api/worktrees` (hq-fe-api-r.8). Optional: when `GT_TOWN_ROOT` is
    // unset (e.g. CI, in-tree tests) the handler short-circuits to an empty list rather than
    // probing the current working directory — the gateway must not invent a default rig path.
    let town_root = std::env::var("GT_TOWN_ROOT")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|s| Arc::new(std::path::PathBuf::from(s)));

    // hq-fe-api-r.12: shared worktree-snapshot broadcast for `GET /api/worktrees/stream`.
    // One polling task per process shells `git worktree list` + `status` + `log` every 2s
    // and fans the snapshot out to every connected client. Skipped when no town root is
    // configured so the SSE endpoint responds 503 instead of hanging.
    let worktrees_stream = town_root.as_ref().map(|root| {
        let (tx, _rx) = tokio::sync::broadcast::channel::<Vec<gt_web::dto::WorktreeDto>>(8);
        let root = root.clone();
        let tx_for_task = tx.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(2));
            loop {
                ticker.tick().await;
                match gt_web::collect_worktrees(root.as_path()).await {
                    Ok(snap) => {
                        // `send` returns Err when there are no receivers; that's expected
                        // before any client connects, and harmless — we just drop the frame.
                        let _ = tx_for_task.send(snap);
                    }
                    Err(e) => {
                        eprintln!("[gt-web] worktrees poll failed: {}", e.message);
                    }
                }
            }
        });
        tx
    });

    // hq-fe-api-w.6 + .8: production polecat control (tmux kill-session + send-keys).
    // A single shared `TmuxCli` matches the supervisor/lifecycle convention (same
    // default tmux server, no socket drift), so kill/interrupt land on the same session
    // the spawner created.
    let tmux: Arc<dyn gt_polecat::Tmux> = Arc::new(gt_polecat::TmuxCli::new());
    let control: Arc<dyn PolecatControl> = Arc::new(TmuxPolecatControl::new(tmux.clone()));

    // hq-fe-api-w.7: production polecat respawner. Builds a dedicated `PolecatLifecycle`
    // off the same env the composition root reads — restart-via-HTTP and restart-via-
    // supervisor produce identically-shaped polecats.
    let respawner: Arc<dyn PolecatRespawner> = {
        let lifecycle = Arc::new(gt_polecat::PolecatLifecycle::new(
            Box::new(gt_polecat::TmuxCli::new()),
            gt_root::spawn_template_from_env(),
        ));
        Arc::new(LifecyclePolecatRespawner::new(tmux.clone(), lifecycle))
    };

    // hq-fe-api-w.5: production issue commenter. Shares the same `Arc<DoltIssues>`
    // the `/api/issues` reader uses so a comment appended via HTTP is visible to
    // the next snapshot without a separate connection pool.
    let commenter: Option<Arc<dyn IssueCommenter>> = issues
        .clone()
        .map(|i| Arc::new(DoltIssueCommenter::new(i)) as Arc<dyn IssueCommenter>);

    let state = AppState {
        beads,
        sessions,
        merges: merge_repo,
        agent_events: root.agent_events.clone(),
        events: root.events_sender(),
        town_root,
        issues,
        // hq-fe-api-w.10: share the running root's CommandBus so HTTP write routes
        // dispatch through the same actors gt-mcp drives.
        bus: Some(root.commands()),
        worktrees_stream,
        control: Some(control),
        respawner: Some(respawner),
        commenter,
    };

    // Idempotency-Key cache (hq-fe-api-w.2). TTL overridable via
    // `GT_WEB_IDEMPOTENCY_TTL_SECS`; the bead pins the default at 10 minutes.
    let idem_ttl = std::env::var("GT_WEB_IDEMPOTENCY_TTL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(std::time::Duration::from_secs)
        .unwrap_or(gt_web::idempotency::DEFAULT_TTL);
    let idem_store = gt_web::IdempotencyStore::new(idem_ttl, gt_web::idempotency::DEFAULT_MAX_ENTRIES);
    let app = gt_web::router_with_idempotency(state, auth, audit, gate, idem_store);
    let listener = tokio::net::TcpListener::bind(bind).await.expect("bind gt-web");
    eprintln!(
        "[gt-web] up on {bind} — event log: {}",
        root.log_path().display()
    );

    // Graceful shutdown (paso 8.5): SIGTERM / SIGINT closes the listener, axum drains the
    // in-flight requests, then we tear down the background tasks in dependency order
    // (broadcast producers before consumers) so no events are dropped from the audit relay.
    let _ = axum::serve(listener, app)
        .with_graceful_shutdown(wait_for_signal("gt-web"))
        .await;

    eprintln!("[gt-web] HTTP listener stopped — draining background tasks");
    // Stop the reactor first: that drops its broadcast Sender, which signals the audit
    // consumer and the sessions projector to drain and exit cleanly via `RecvError::Closed`.
    root.shutdown();
    // Both background consumers exit on broadcast `Closed` — await, never abort (hq-8iur.5).
    let _ = sessions_projector.await;
    if let Some(task) = audit_task {
        let _ = task.await;
    }
    eprintln!("[gt-web] shutdown complete");
}

/// Wait for SIGTERM or SIGINT. Returns the unit when the first arrives. If signal install
/// fails (non-Unix), falls back to a never-resolving future so axum keeps serving until
/// the process is killed externally — better than auto-exit on startup.
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

/// Connect to PG audit if `GT_PG_AUDIT_URL` is set. `Arc<PgAudit>` so the same handle backs
/// the relay task AND the readyz probe.
async fn connect_pg_audit() -> Option<Arc<PgAudit>> {
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
    Some(Arc::new(audit))
}

/// Spawn the broadcast → Postgres relay task. The task exits on `Closed` (the root dropped
/// its broadcast Sender on shutdown), so awaiting the join handle blocks until the final
/// `append` lands — no events lost on a clean SIGTERM.
fn spawn_pg_audit_task(
    audit: Arc<PgAudit>,
    mut rx: tokio::sync::broadcast::Receiver<gt_audit::EventRecord>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
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
    })
}

// Bounds anchor for `RootHandle<R>` generic so the import stays load-bearing in lib.rs
// signatures and the IDE doesn't lint it as unused when neither branch is exercised.
#[allow(dead_code)]
fn _root_bounds<R: BeadRepository + Clone + 'static>(_: &RootHandle<R>) {}
