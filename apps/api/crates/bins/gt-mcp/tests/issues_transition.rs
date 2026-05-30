//! Gate test for hq-mcp-issues.4: the `issues.transition.{validate,execute}` MCP tools.
//!
//! Covers the frontier-only path (target parsing, scope check, audit). The DB-side state
//! machine is exercised by `gt-store-dolt/tests/issues_contract.rs::transition_state_machine_round_trip`.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use gt_mcp::{
    audit::{AuditEvent, AuditSink, InMemoryAudit, Outcome},
    auth::Scope,
    McpService, TransitionIssue,
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
async fn validate_accepts_known_targets() {
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    for target in ["open", "working", "closed"] {
        svc.run_transition_issue(
            "issues.transition.validate",
            TransitionIssue {
                id: "hq-test-1".into(),
                target: target.into(),
            },
            true,
        )
        .await
        .unwrap_or_else(|e| panic!("validate {target} should pass: {e}"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_rejects_unknown_target() {
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    let err = svc
        .run_transition_issue(
            "issues.transition.validate",
            TransitionIssue {
                id: "hq-1".into(),
                target: "bogus".into(),
            },
            true,
        )
        .await
        .expect_err("unknown target must reject");
    assert!(err.to_string().contains("unknown target status"), "got `{err}`");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_rejects_empty_id() {
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    let err = svc
        .run_transition_issue(
            "issues.transition.validate",
            TransitionIssue {
                id: String::new(),
                target: "working".into(),
            },
            true,
        )
        .await
        .expect_err("empty id must reject");
    assert!(err.to_string().contains("issue id is empty"), "got `{err}`");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_without_backend_surfaces_clear_error() {
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    let err = svc
        .run_transition_issue(
            "issues.transition.execute",
            TransitionIssue {
                id: "hq-1".into(),
                target: "closed".into(),
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
    let scope = narrow_scope("read-only", &["issues.transition.validate"]);
    let svc = full_service(scope, audit.clone());
    let err = svc
        .run_transition_issue(
            "issues.transition.execute",
            TransitionIssue {
                id: "hq-1".into(),
                target: "closed".into(),
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
            AuditEvent::Unauthorized { tool, .. } if tool == "issues.transition.execute"
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
            } if tool == "issues.transition.execute"
        )),
        "execute should not have recorded an Ok outcome",
    );
}
