//! Gate test for `hq-mcp-onboard.8`: `meta.report_gap` mints a `hq-gap-…` bead through
//! the same scheduling actor as `scheduling.create_bead`, with id derived from the
//! operation name + a coarse timestamp suffix.

use std::collections::HashMap;
use std::sync::Arc;

use gt_beads::{BeadRepository, BeadStatus, InMemoryBeads};
use gt_mcp::{
    audit::{AuditEvent, AuditSink, InMemoryAudit, Outcome},
    auth::Scope,
    McpService, ReportGap,
};
use tokio::sync::mpsc;

struct AuditCapture {
    inner: InMemoryAudit,
}

impl AuditSink for AuditCapture {
    fn record(&self, event: AuditEvent) {
        self.inner.record(event);
    }
}

fn service_with_beads(
    scope: Scope,
    beads: Arc<InMemoryBeads>,
    audit: Arc<dyn AuditSink>,
) -> McpService {
    let agent = gt_agent::actor::spawn(16);
    let (merge_tx, _merge_rx) = mpsc::channel(16);
    let merge = gt_merge::actor::spawn(gt_merge::InMemoryMergeRepo::default(), merge_tx);
    let (sched_tx, _sched_rx) = mpsc::channel(16);
    let sched = gt_scheduling::actor::spawn(beads, sched_tx, 4);
    let (patrol_tx, _patrol_rx) = mpsc::channel(16);
    let patrol = gt_patrol::actor::spawn(gt_patrol::InMemoryPatrolRepo::default(), patrol_tx);
    let (orch_tx, _orch_rx) = mpsc::channel(16);
    let orch = gt_orchestration::actor::spawn(
        gt_orchestration::InMemoryOrchRepo::default(),
        orch_tx,
    );
    let (quota_tx, _quota_rx) = mpsc::channel(16);
    let quota = gt_quota::actor::spawn(quota_tx, HashMap::new());
    McpService::new(agent, merge, sched, patrol, orch, quota, scope, audit)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn report_gap_mints_hq_gap_bead_via_scheduling_actor() {
    let beads = Arc::new(InMemoryBeads::default());
    let audit_inner = InMemoryAudit::new();
    let audit: Arc<dyn AuditSink> = Arc::new(AuditCapture { inner: audit_inner.clone() });
    let svc = service_with_beads(Scope::admin("max"), beads.clone(), audit);

    let args = ReportGap {
        operation: "issues.update.execute".into(),
        notes: Some("title/description/priority/assignee/notes".into()),
        priority: Some(1),
    };
    let result = svc
        .run_report_gap(args)
        .await
        .expect("meta.report_gap should succeed");

    assert!(!result.is_error.unwrap_or(false), "tool returned isError=true");
    let text = match result.content.first().and_then(|c| c.as_text()) {
        Some(t) => t.text.clone(),
        None => panic!("expected text content"),
    };
    let body: serde_json::Value = serde_json::from_str(&text).expect("response is JSON");
    let bead_id = body.get("bead").and_then(|v| v.as_str()).unwrap().to_string();
    assert!(
        bead_id.starts_with("hq-gap-issues-update-execute-"),
        "bead id should be hq-gap-<slug>-<ts>, got {bead_id}"
    );
    assert_eq!(body["priority"], serde_json::json!(1));

    let stored = beads.get(&bead_id).await.expect("bead written to repo").unwrap();
    assert_eq!(stored.status, BeadStatus::Pending);
    assert_eq!(stored.priority, 1);
    assert!(stored.title.starts_with("gap: issues.update.execute"));

    let audit_events = audit_inner.snapshot();
    assert!(
        audit_events.iter().any(|e| matches!(
            e,
            AuditEvent::Invoked { tool, outcome: Outcome::Ok, .. } if tool == "meta.report_gap"
        )),
        "expected an Invoked record for meta.report_gap, got {audit_events:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn report_gap_rejects_empty_operation() {
    let beads = Arc::new(InMemoryBeads::default());
    let audit: Arc<dyn AuditSink> = Arc::new(InMemoryAudit::new());
    let svc = service_with_beads(Scope::admin("max"), beads, audit);

    let err = svc
        .run_report_gap(ReportGap {
            operation: "   ".into(),
            notes: None,
            priority: None,
        })
        .await
        .expect_err("empty operation must fail validation");
    assert!(
        err.to_string().contains("operation is empty"),
        "expected validation error, got {err}"
    );
}
