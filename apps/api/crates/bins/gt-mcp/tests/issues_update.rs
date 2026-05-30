//! Gate test for hq-mcp-issues.3: the `issues.update.{validate,execute}` MCP tools.
//!
//! Covers the frontier-only path (scope check + shape validation + audit). The Dolt
//! patch is exercised by `gt-store-dolt/tests/issues_contract.rs::update_patches_visible_fields_and_commits`;
//! without a backend wired, `execute` must surface `issues backend not wired`.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use gt_mcp::{
    audit::{AuditEvent, AuditSink, InMemoryAudit, Outcome},
    auth::Scope,
    McpService, UpdateIssue,
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

fn ok_payload() -> UpdateIssue {
    UpdateIssue {
        id: "hq-test-1".into(),
        title: Some("new title".into()),
        description: None,
        design: None,
        acceptance_criteria: None,
        notes: None,
        priority: Some(1),
        issue_type: None,
        assignee: Some("alice".into()),
        owner: None,
        external_ref: None,
    }
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
async fn validate_accepts_well_shaped_patch() {
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    svc.run_update_issue("issues.update.validate", ok_payload(), true)
        .await
        .expect("validate ok");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_rejects_empty_id() {
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    let mut bad = ok_payload();
    bad.id = String::new();
    let err = svc
        .run_update_issue("issues.update.validate", bad, true)
        .await
        .expect_err("empty id must be rejected");
    assert!(err.to_string().contains("issue id is empty"), "got `{err}`");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_rejects_empty_patch() {
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    let mut empty = ok_payload();
    empty.title = None;
    empty.priority = None;
    empty.assignee = None;
    let err = svc
        .run_update_issue("issues.update.validate", empty, true)
        .await
        .expect_err("empty patch must be rejected");
    assert!(err.to_string().contains("nothing to update"), "got `{err}`");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_rejects_out_of_range_priority() {
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    let mut bad = ok_payload();
    bad.priority = Some(9);
    let err = svc
        .run_update_issue("issues.update.validate", bad, true)
        .await
        .expect_err("priority>2 must be rejected");
    assert!(err.to_string().contains("priority"), "got `{err}`");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_rejects_empty_title_string() {
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    let mut bad = ok_payload();
    bad.title = Some(String::new());
    let err = svc
        .run_update_issue("issues.update.validate", bad, true)
        .await
        .expect_err("title='' must be rejected");
    assert!(err.to_string().contains("title is empty"), "got `{err}`");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_without_backend_surfaces_clear_error() {
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    let err = svc
        .run_update_issue("issues.update.execute", ok_payload(), false)
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
    let scope = narrow_scope("read-only", &["issues.update.validate"]);
    let svc = full_service(scope, audit.clone());
    let err = svc
        .run_update_issue("issues.update.execute", ok_payload(), false)
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
            AuditEvent::Unauthorized { tool, .. } if tool == "issues.update.execute"
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
            } if tool == "issues.update.execute"
        )),
        "execute should not have recorded an Ok outcome",
    );
}
