//! `gt-web` binary — the read-side process. Boots the composition root (`bins/gt`) plus the
//! Axum router; the single tokio runtime lives here per `docs/01-architecture.md`.
//!
//! Persistence (hq-j9ou): when `GT_DOLT_URL` is set, the bead repo behind `AppState.beads`
//! and behind the root is the real Dolt-backed `DoltBeads`; otherwise the bin keeps the
//! in-memory port so the API stays runnable on a host without Dolt. `GT_PG_AUDIT_URL`
//! mirrors every appended `EventRecord` into Postgres (canonical audit per docs/04).
//!
//! Effects (hq-7pdl.1): wired with the production [`RealEffects`] adapter (`gt sling` child
//! processes + `QuotaCommand::Rotate` chain). `gt` binary path comes from `GT_BIN`.
//!
//! IAM (hq-7pdl.2): bearer-token middleware sits in front of every route. The secret is
//! read from `GT_WEB_TOKEN`; without it the bin refuses to start unless
//! `GT_WEB_AUTH=disabled` is set explicitly (intended for in-tree dev only). Every accepted
//! / rejected request lands in the shared `events.jsonl` as a `web.*` frontier-audit record.

use std::path::PathBuf;
use std::sync::Arc;

use gt_agent::InMemorySessions;
use gt_audit::JsonlWriter;
use gt_beads::{BeadRepository, InMemoryBeads};
use gt_root::{spawn, RealEffects, RootConfig, RootHandle, SystemClock};
use gt_store_dolt::DoltBeads;
use gt_store_pg::PgAudit;
use gt_telemetry::{init as init_telemetry, TelemetryConfig};
use gt_web::{router, AppState, AuthConfig, JsonlWebAudit, WebAuditSink};

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

        match std::env::var("GT_DOLT_URL").ok() {
            Some(url) => {
                let dolt = DoltBeads::connect(&url).expect("connect Dolt");
                dolt.ensure_schema().await.expect("Dolt ensure_schema");
                eprintln!("[gt-web] beads: Dolt @ {url}");
                serve(Arc::new(dolt), &log_path, &bind, gt_bin, auth).await;
            }
            None => {
                eprintln!("[gt-web] beads: in-memory (set GT_DOLT_URL for Dolt persistence)");
                serve(
                    Arc::new(InMemoryBeads::default()),
                    &log_path,
                    &bind,
                    gt_bin,
                    auth,
                )
                .await;
            }
        }
    });
}

async fn serve<R>(
    beads: Arc<R>,
    log_path: &str,
    bind: &str,
    gt_bin: PathBuf,
    auth: AuthConfig,
) where
    R: BeadRepository + Send + Sync + 'static,
    Arc<R>: BeadRepository + Clone + 'static,
{
    let sessions = Arc::new(InMemorySessions::default());

    let (effects, quota_slot) = RealEffects::new(gt_bin);
    let root = spawn(
        beads.clone(),
        effects,
        SystemClock,
        log_path,
        RootConfig::default(),
    );
    let _ = quota_slot.set(root.quota.clone());

    let audit_task = spawn_pg_audit(&root).await;

    // Frontier audit writes to the same events.jsonl the reactor appends to — the boundary's
    // who-consulted-what records share the system log (`web.*` frontier-audit prefix).
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
