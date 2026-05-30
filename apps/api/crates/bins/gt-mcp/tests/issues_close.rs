//! Gate test for hq-mcp-issues.5: the `issues.close.{validate,execute}` MCP tools.
//!
//! Covers the frontier-only path (validation, attribution fallback, scope, audit). The
//! DB-side close is exercised by
//! `gt-store-dolt/tests/issues_contract.rs::close_stamps_attribution_and_rejects_double`.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use gt_mcp::{
    audit::{AuditEvent, AuditSink, InMemoryAudit, Outcome},
    auth::Scope,
    CloseIssue, McpService,
};
use tokio::sync::mpsc;

fn full_service(scope: Scope, audit: Arc<InMemoryAudit>) -> McpService {
    let agent = gt_agent::actor::spawn(16);
    let (merge_tx, _merge_rx) = mpsc::channel(16);
    let merge = gt_merge::actor::spawn(gt_merge::InMemoryMergeRepo::default(), merge_tx);
    let (sched_tx, _sched_rx) = mpsc::channel(16);
    let sched =
        gt_scheduling::actor::spawn(Arc::new(gt_beads::InMemoryBeads::default()), sched_tx, 4);
    let (patrol_tx, _patrol_rx) = mpsc::channel(16);
    let patrol = gt_patrol::actor::spawn(gt_patrol::InMemoryPatrolRepo::default(), patrol_tx);
    let (orch_tx, _orch_rx) = mpsc::channel(16);
    let orch = gt_orchestration::actor::spawn(gt_orchestration::InMemoryOrchRepo::default(), orch_tx);
    let (quota_tx, _quota_rx) = mpsc::channel(16);
    let quota = gt_quota::actor::spawn(quota_tx, HashMap::new());
    let sink: Arc<dyn AuditSink> = audit;
    McpService::new(agent, merge, sched, patrol, orch, quota, scope, sink)
}

fn narrow_scope(actor: &str, allowed: &[&str]) -> Scope {
    let mut allow = BTreeSet::new();
    for pat in allowed {
        allow.insert((*pat).into());
    }
    Scope {
        actor: actor.into(),
        allow,
        validate_only: false,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_accepts_well_shaped_payload() {
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    svc.run_close_issue(
        "issues.close.validate",
        CloseIssue {
            id: "hq-1".into(),
            closed_by_session: None,
        },
        true,
    )
    .await
    .expect("default-session validate ok");

    svc.run_close_issue(
        "issues.close.validate",
        CloseIssue {
            id: "hq-1".into(),
            closed_by_session: Some("explicit-session".into()),
        },
        true,
    )
    .await
    .expect("explicit-session validate ok");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_rejects_empty_id() {
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    let err = svc
        .run_close_issue(
            "issues.close.validate",
            CloseIssue {
                id: String::new(),
                closed_by_session: None,
            },
            true,
        )
        .await
        .expect_err("empty id must reject");
    assert!(err.to_string().contains("issue id is empty"), "got `{err}`");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_rejects_empty_session_string() {
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    let err = svc
        .run_close_issue(
            "issues.close.validate",
            CloseIssue {
                id: "hq-1".into(),
                closed_by_session: Some(String::new()),
            },
            true,
        )
        .await
        .expect_err("Some(\"\") session must reject");
    assert!(err.to_string().contains("closed_by_session is empty"), "got `{err}`");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_without_backend_surfaces_clear_error() {
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    let err = svc
        .run_close_issue(
            "issues.close.execute",
            CloseIssue {
                id: "hq-1".into(),
                closed_by_session: None,
            },
            false,
        )
        .await
        .expect_err("execute must fail without backend");
    assert!(
        err.to_string().contains("issues backend not wired"),
        "got `{err}`",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn narrow_scope_rejects_execute_and_audits_unauthorized() {
    let audit = Arc::new(InMemoryAudit::new());
    let scope = narrow_scope("read-only", &["issues.close.validate"]);
    let svc = full_service(scope, audit.clone());
    let err = svc
        .run_close_issue(
            "issues.close.execute",
            CloseIssue {
                id: "hq-1".into(),
                closed_by_session: None,
            },
            false,
        )
        .await
        .expect_err("scope must reject execute");
    assert!(
        err.to_string().to_lowercase().contains("scope")
            || err.to_string().to_lowercase().contains("not in scope"),
        "got `{err}`",
    );

    let events = audit.snapshot();
    assert!(
        events.iter().any(|e| matches!(
            e,
            AuditEvent::Unauthorized { tool, .. } if tool == "issues.close.execute"
        )),
        "unauthorized audit row missing",
    );
    assert!(
        events.iter().all(|e| !matches!(
            e,
            AuditEvent::Invoked {
                tool,
                outcome: Outcome::Ok,
                ..
            } if tool == "issues.close.execute"
        )),
        "execute should not have recorded an Ok outcome",
    );
}
