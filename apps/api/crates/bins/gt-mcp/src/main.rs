//! `gt-mcp` binary entry. Boots the composition root (`bins/gt`) and serves the MCP
//! service over stdio or streamable-HTTP, sharing the root's `AgentHandle` so MCP tool
//! calls drive the same session actor the root drives — not an isolated copy (Paso 6.f.3).
//! Every invocation is persisted to the shared events.jsonl through the gt-audit writer
//! (Paso 6.f.4); when `GT_PG_AUDIT_URL` is set the same records also land in Postgres.
//!
//! Persistence (hq-j9ou): `GT_DOLT_URL` swaps the in-memory bead repo for `DoltBeads`.

use std::sync::Arc;

use rmcp::transport::stdio;
use rmcp::ServiceExt;

use gt_audit::JsonlWriter;
use gt_beads::{BeadRepository, InMemoryBeads};
use gt_merge::InMemoryMergeRepo;
use gt_orchestration::InMemoryOrchRepo;
use gt_patrol::InMemoryPatrolRepo;
use gt_root::{spawn, LogEffects, RootConfig, RootHandle, SystemClock};
use gt_store_dolt::{DoltBeads, DoltIssues, DoltMerge, DoltOrch, DoltPatrol, DoltSessions};
use gt_store_pg::PgAudit;
use gt_telemetry::{init as init_telemetry, TelemetryConfig};

use gt_mcp::{
    audit::AuditSink,
    auth::{ResolveScope, Scope},
    IssuesRead, JsonlAudit, McpService, ScopeConfig, SessionsRead,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _telemetry = init_telemetry(TelemetryConfig::from_env("gt-mcp"))
        .map_err(|e| eprintln!("[gt-mcp] telemetry init: {e} (continuing without exporter)"))
        .ok();

    let log_path =
        std::env::var("GT_EVENT_LOG").unwrap_or_else(|_| "/tmp/gt.events.jsonl".to_string());

    match std::env::var("GT_DOLT_URL").ok() {
        Some(url) => {
            let dolt = DoltBeads::connect(&url)?;
            dolt.ensure_schema().await?;
            let dolt_sessions = DoltSessions::connect(&url)?;
            dolt_sessions.ensure_schema().await?;
            let dolt_merge = DoltMerge::connect(&url)?;
            dolt_merge.ensure_schema().await?;
            let dolt_patrol = DoltPatrol::connect(&url)?;
            dolt_patrol.ensure_schema().await?;
            let dolt_orch = DoltOrch::connect(&url)?;
            dolt_orch.ensure_schema().await?;
            // hq-mcp-issues.1 — probe the canonical `issues` table so the
            // `gt://issues` resource has a backend. Schema is owned by `bd`,
            // so this is a presence check, not a CREATE.
            let dolt_issues = DoltIssues::connect(&url)?;
            dolt_issues.ensure_schema().await?;
            eprintln!("[gt-mcp] beads + sessions + merge/patrol/orch + issues: Dolt @ {url}");
            serve(
                Arc::new(dolt),
                Some(Arc::new(dolt_sessions)),
                Some(Arc::new(dolt_issues)),
                Arc::new(dolt_merge),
                Arc::new(dolt_patrol),
                Arc::new(dolt_orch),
                log_path,
            )
            .await
        }
        None => {
            eprintln!(
                "[gt-mcp] domain state + sessions: in-memory (set GT_DOLT_URL for Dolt persistence)"
            );
            serve(
                Arc::new(InMemoryBeads::default()),
                None,
                None,
                Arc::new(InMemoryMergeRepo::default()),
                Arc::new(InMemoryPatrolRepo::default()),
                Arc::new(InMemoryOrchRepo::default()),
                log_path,
            )
            .await
        }
    }
}

async fn serve<R, MR, PR, OR>(
    beads: Arc<R>,
    dolt_sessions: Option<Arc<DoltSessions>>,
    dolt_issues: Option<Arc<DoltIssues>>,
    merge_repo: Arc<MR>,
    patrol_repo: Arc<PR>,
    orch_repo: Arc<OR>,
    log_path: String,
) -> anyhow::Result<()>
where
    R: BeadRepository + Send + Sync + 'static,
    Arc<R>: BeadRepository + Clone + 'static,
    MR: gt_merge::MergeRepository + 'static,
    Arc<MR>: gt_merge::MergeRepository + 'static,
    PR: gt_patrol::PatrolRepository + 'static,
    Arc<PR>: gt_patrol::PatrolRepository + 'static,
    OR: gt_orchestration::OrchRepository + 'static,
    Arc<OR>: gt_orchestration::OrchRepository + 'static,
{
    let root = spawn(
        beads,
        merge_repo,
        patrol_repo,
        orch_repo,
        LogEffects,
        SystemClock,
        &log_path,
        RootConfig::default(),
    );
    let audit_task = spawn_pg_audit(&root).await;

    // Frontier audit lands in the same event log as the domain events. A second JsonlWriter on
    // the same path is safe: each append takes the file lock, so lines never interleave. The
    // `mcp.` prefix marks these as observability — domain replay skips it (`replay_gt`).
    let audit: Arc<dyn AuditSink> =
        Arc::new(JsonlAudit::new(Arc::new(JsonlWriter::new(&log_path))));

    // Resolve this connection's scope from the per-actor config file (TOML or JSON). There is
    // no hardcoded admin: with no `GT_MCP_SCOPE_CONFIG`, or an actor absent from it, the scope
    // is closed (deny all) — Paso 6.f.10, deny by default.
    let actor = std::env::var("GT_MCP_ACTOR").unwrap_or_else(|_| "mcp-local".to_string());
    let scope = match std::env::var("GT_MCP_SCOPE_CONFIG") {
        Ok(path) => ScopeConfig::load(&path)?.resolve(&actor),
        Err(_) => {
            eprintln!(
                "[gt-mcp] GT_MCP_SCOPE_CONFIG unset; actor '{actor}' gets a closed scope (deny all)"
            );
            Scope::denied(&actor)
        }
    };

    // Share the root's domain actors, not isolated `actor::spawn`s. MCP tool calls drive the
    // same agent + merge + scheduling + patrol + orchestration + quota actors the root drives,
    // so their events land in the shared log.
    //
    // Sessions read-side (hq-u955): when `GT_DOLT_URL` is set the `gt://agent/sessions`
    // resource reads the canonical Dolt table (the one `gt sling` writes to); otherwise it
    // falls back to the actor snapshot — same path the tests cover.
    let sessions = match dolt_sessions {
        Some(d) => SessionsRead::Dolt(d),
        None => SessionsRead::Actor(root.agent.clone()),
    };
    let service = McpService::with_sessions(
        root.agent.clone(),
        sessions,
        root.merge.clone(),
        root.sched.clone(),
        root.patrol.clone(),
        root.orch.clone(),
        root.quota.clone(),
        scope,
        audit,
        Some(root.agent_events.clone()),
    );
    // hq-mc72.12.30 — the composition root now owns a hydrated `RigHandle` (spawned +
    // relay-drained in `bins/gt::root`). Share it so the `rig.*` tools + `gt://rigs` resource
    // drive the same catalog actor the root persists — its `RigEvent`s land in the shared log
    // and stream out over SSE like every other domain.
    let service = service.with_rig(root.rig.clone());

    // hq-mcp-issues.1 — wire the canonical issues snapshot when Dolt is
    // configured. Without `GT_DOLT_URL` the reader stays `none` and the
    // `gt://issues` resource returns `[]`, matching the `gt://rigs` shape.
    let issues = match dolt_issues {
        Some(d) => IssuesRead::dolt(d),
        None => IssuesRead::none(),
    };
    let service = service.with_issues(issues);

    // Transport selection (Paso 6.f.12). `GT_MCP_TRANSPORT=http` serves the streamable-HTTP
    // transport (bind via `GT_MCP_HTTP_BIND`); anything else keeps the default stdio transport.
    match std::env::var("GT_MCP_TRANSPORT").as_deref() {
        Ok("http") => {
            let bind =
                std::env::var("GT_MCP_HTTP_BIND").unwrap_or_else(|_| "127.0.0.1:8765".to_string());
            gt_mcp::http::serve(service, &bind).await?;
        }
        _ => {
            let handle = service.serve(stdio()).await?;
            handle.waiting().await?;
        }
    }

    if let Some(task) = audit_task {
        task.abort();
    }
    root.shutdown();
    Ok(())
}

async fn spawn_pg_audit<R>(root: &RootHandle<R>) -> Option<tokio::task::JoinHandle<()>>
where
    R: BeadRepository + Clone + 'static,
{
    let url = std::env::var("GT_PG_AUDIT_URL").ok()?;
    let audit = match PgAudit::connect(&url).await {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[gt-mcp] PG audit disabled — connect failed: {e}");
            return None;
        }
    };
    if let Err(e) = gt_store_pg::ensure_schema(audit.pool()).await {
        eprintln!("[gt-mcp] PG audit disabled — migrations failed: {e}");
        return None;
    }
    eprintln!("[gt-mcp] audit: Postgres @ {url}");
    let mut rx = root.subscribe_events();
    Some(tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(rec) => {
                    if let Err(e) = audit.append(&rec).await {
                        eprintln!("[gt-mcp] PG audit append failed ({}): {e}", rec.kind);
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    eprintln!("[gt-mcp] PG audit lagged by {n} events (catching up)");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    }))
}
