//! Gate test for hq-mcp-issues.2: the `issues.create.{validate,execute}` MCP tools.
//!
//! These exercise the frontier-only path (scope check + shape validation + audit). The
//! actual Dolt write is gated by `GT_DOLT_URL` and covered by
//! `gt-store-dolt/tests/issues_contract.rs::insert_commits_atomic`; without a real
//! backend wired, `execute` must surface `issues backend not wired` so misconfigured
//! deploys fail loudly instead of silently dropping inserts.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use gt_mcp::{
    audit::{AuditEvent, AuditSink, InMemoryAudit, Outcome},
    auth::Scope,
    CreateIssue, Domain, McpService, Role,
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

fn ok_payload() -> CreateIssue {
    CreateIssue {
        id: "hq-test-1".into(),
        title: "test create".into(),
        description: String::new(),
        design: String::new(),
        acceptance_criteria: String::new(),
        notes: String::new(),
        priority: 1,
        issue_type: "task".into(),
        created_by: "claude-host".into(),
        external_ref: Some("hq-test".into()),
        assignee: None,
        owner: None,
        // hq-taxon.2 — non-empty domain[] is now mandatory. `docs.spec` is
        // an anyone-allowed layer so role_scope=None tests still pass.
        domain: vec![Domain::DocsSpec],
        surface: Vec::new(),
        depends_on: Vec::new(),
        role_scope: None,
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
async fn validate_accepts_well_shaped_payload() {
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    svc.run_create_issue("issues.create.validate", ok_payload(), true)
        .await
        .expect("validate ok");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_rejects_empty_required_fields() {
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    let mut bad = ok_payload();
    bad.id = String::new();
    let err = svc
        .run_create_issue("issues.create.validate", bad, true)
        .await
        .expect_err("empty id must be rejected");
    assert!(err.to_string().contains("issue id is empty"), "got `{err}`");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_rejects_out_of_range_priority() {
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    let mut bad = ok_payload();
    bad.priority = 9;
    let err = svc
        .run_create_issue("issues.create.validate", bad, true)
        .await
        .expect_err("priority>2 must be rejected");
    assert!(err.to_string().contains("priority"), "got `{err}`");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_without_backend_surfaces_clear_error() {
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    let err = svc
        .run_create_issue("issues.create.execute", ok_payload(), false)
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
    // Scope that allows only the validate variant, not the execute one.
    let scope = narrow_scope("read-only", &["issues.create.validate"]);
    let svc = full_service(scope, audit.clone());
    let err = svc
        .run_create_issue("issues.create.execute", ok_payload(), false)
        .await
        .expect_err("scope must reject execute");
    assert!(
        err.to_string().to_lowercase().contains("scope")
            || err.to_string().to_lowercase().contains("not in scope"),
        "got `{err}`",
    );

    let events = audit.snapshot();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AuditEvent::Unauthorized { tool, .. } if tool == "issues.create.execute")),
        "unauthorized audit row missing from {events:?}",
    );
    // The unauthorized branch must not have recorded an Invoked.Ok outcome.
    assert!(
        events.iter().all(|e| !matches!(
            e,
            AuditEvent::Invoked {
                tool,
                outcome: Outcome::Ok,
                ..
            } if tool == "issues.create.execute"
        )),
        "execute reached the actor despite scope denial",
    );
}

// ---- hq-taxon.2 taxonomy validation -----------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_rejects_empty_domain() {
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    let mut bad = ok_payload();
    bad.domain.clear();
    let err = svc
        .run_create_issue("issues.create.validate", bad, true)
        .await
        .expect_err("empty domain[] must be rejected (hq-taxon.2)");
    assert!(err.to_string().contains("at least one domain"), "got `{err}`");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_rejects_self_cycle_in_depends_on() {
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    let mut bad = ok_payload();
    bad.depends_on = vec!["hq-test-1".into()]; // matches the bead's own id
    let err = svc
        .run_create_issue("issues.create.validate", bad, true)
        .await
        .expect_err("self-cycle must be rejected");
    assert!(err.to_string().contains("self-cycle"), "got `{err}`");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_rejects_duplicate_depends_on() {
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    let mut bad = ok_payload();
    bad.depends_on = vec!["hq-a".into(), "hq-a".into()];
    let err = svc
        .run_create_issue("issues.create.validate", bad, true)
        .await
        .expect_err("duplicate depends_on must be rejected");
    assert!(err.to_string().contains("more than once"), "got `{err}`");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_rejects_role_outside_permitted_domains() {
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    let mut bad = ok_payload();
    bad.role_scope = Some(Role::Sheriff);
    bad.domain = vec![Domain::OrchQuota]; // sheriff does not own quota — see doc 14 §3.5
    let err = svc
        .run_create_issue("issues.create.validate", bad, true)
        .await
        .expect_err("sheriff on quota must be rejected");
    let msg = err.to_string();
    assert!(msg.contains("sheriff"), "got `{err}`");
    assert!(msg.contains("orch.quota"), "got `{err}`");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_accepts_role_with_in_scope_domain() {
    let audit = Arc::new(InMemoryAudit::new());
    let svc = full_service(Scope::admin("max"), audit.clone());
    let mut ok = ok_payload();
    ok.role_scope = Some(Role::Refinery);
    ok.domain = vec![Domain::OrchMerge, Domain::OrchQuota];
    svc.run_create_issue("issues.create.validate", ok, true)
        .await
        .expect("refinery on merge+quota must be accepted");
}
