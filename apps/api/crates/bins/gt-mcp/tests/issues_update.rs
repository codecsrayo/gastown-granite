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
    ClaimIssue, McpService, UpdateIssue,
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
        domain: None,
        surface: None,
        depends_on: None,
        expected_version: None,
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
async fn validate_accepts_surface_only_overwrite() {
    // The core repoint case: a patch that touches ONLY surface_json must count
    // as non-empty and pass shape validation (regression for the gap where the
    // JSON-array columns were unreachable from issues.update).
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    let mut p = ok_payload();
    p.title = None;
    p.priority = None;
    p.assignee = None;
    p.surface = Some(vec!["crates/domain/platform/gt-web-context/src/lib.rs".into()]);
    svc.run_update_issue("issues.update.validate", p, true)
        .await
        .expect("surface-only patch is a valid non-empty update");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_accepts_domain_and_depends_on_overwrite() {
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    let mut p = ok_payload();
    p.domain = Some(vec![gt_mcp::taxonomy::Domain::OrchMerge]);
    p.depends_on = Some(vec!["hq-other-1".into(), "hq-other-2".into()]);
    svc.run_update_issue("issues.update.validate", p, true)
        .await
        .expect("domain + depends_on overwrite accepted");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_rejects_empty_domain_overwrite() {
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    let mut bad = ok_payload();
    bad.domain = Some(vec![]);
    let err = svc
        .run_update_issue("issues.update.validate", bad, true)
        .await
        .expect_err("empty domain overwrite must be rejected");
    assert!(err.to_string().contains("at least one domain"), "got `{err}`");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_rejects_depends_on_self_cycle() {
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    let mut bad = ok_payload();
    bad.depends_on = Some(vec![bad.id.clone()]);
    let err = svc
        .run_update_issue("issues.update.validate", bad, true)
        .await
        .expect_err("self-cycle must be rejected");
    assert!(err.to_string().contains("self-cycle"), "got `{err}`");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_rejects_duplicate_depends_on() {
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    let mut bad = ok_payload();
    bad.depends_on = Some(vec!["hq-dup-1".into(), "hq-dup-1".into()]);
    let err = svc
        .run_update_issue("issues.update.validate", bad, true)
        .await
        .expect_err("duplicate depends_on must be rejected");
    assert!(err.to_string().contains("more than once"), "got `{err}`");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_accepts_empty_string_clear_of_assignee() {
    // Clearing assignee/owner is expressed as an empty-string overwrite, which
    // must count as a non-empty patch (the repo maps "" -> SQL NULL). Regression
    // for the gap where there was no way to detach an owner/assignee.
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    let mut p = ok_payload();
    p.title = None;
    p.priority = None;
    p.assignee = Some(String::new());
    svc.run_update_issue("issues.update.validate", p, true)
        .await
        .expect("empty-string assignee clear is a valid non-empty patch");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_rejects_empty_surface_entry() {
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    let mut bad = ok_payload();
    bad.surface = Some(vec!["crates/ok".into(), "  ".into()]);
    let err = svc
        .run_update_issue("issues.update.validate", bad, true)
        .await
        .expect_err("empty surface entry must be rejected");
    assert!(err.to_string().contains("empty entry"), "got `{err}`");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_rejects_duplicate_surface() {
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    let mut bad = ok_payload();
    bad.surface = Some(vec!["crates/dup".into(), "crates/dup".into()]);
    let err = svc
        .run_update_issue("issues.update.validate", bad, true)
        .await
        .expect_err("duplicate surface must be rejected");
    assert!(err.to_string().contains("more than once"), "got `{err}`");
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
async fn claim_validate_accepts_well_formed_id() {
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    svc.run_claim_issue("issues.claim.validate", ClaimIssue { id: "hq-c-1".into() }, true)
        .await
        .expect("claim validate ok");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claim_validate_rejects_empty_id() {
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    let err = svc
        .run_claim_issue("issues.claim.validate", ClaimIssue { id: String::new() }, true)
        .await
        .expect_err("empty id must be rejected");
    assert!(err.to_string().contains("issue id is empty"), "got `{err}`");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claim_execute_without_backend_surfaces_clear_error() {
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    let err = svc
        .run_claim_issue("issues.claim.execute", ClaimIssue { id: "hq-c-1".into() }, false)
        .await
        .expect_err("execute must fail without backend");
    assert!(
        err.to_string().contains("issues backend not wired"),
        "got `{err}`",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claim_execute_rejected_by_narrow_scope() {
    let audit = Arc::new(InMemoryAudit::new());
    let scope = narrow_scope("read-only", &["issues.update.validate"]);
    let svc = full_service(scope, audit.clone());
    let err = svc
        .run_claim_issue("issues.claim.execute", ClaimIssue { id: "hq-c-1".into() }, false)
        .await
        .expect_err("scope must reject claim");
    assert!(
        err.to_string().to_lowercase().contains("scope")
            || err.to_string().to_lowercase().contains("not in scope"),
        "got `{err}`",
    );
    assert!(
        audit.snapshot().iter().any(|e| matches!(
            e,
            AuditEvent::Unauthorized { tool, .. } if tool == "issues.claim.execute"
        )),
        "unauthorized audit row missing",
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
