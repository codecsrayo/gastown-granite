//! Gate test for Paso 6.f.8: gt-mcp drives the quota domain end-to-end through the
//! `Command { validate, execute }` path.
//!
//! `McpService::run_quota` is the shared dispatch backing every quota `#[tool]` method.
//! Driving it directly covers the same scope + audit + actor path the wire transport hits.
//!
//! Covers:
//! - A `read_only` scope rejects `quota.sample.execute` and records `Unauthorized`.
//! - A `validate` with an empty account fails cleanly, audited `Invoked { Failed }`.
//! - A full-scope sample/probe/rotate mutates the shared registry and emits exactly
//!   `TokensSampled`, `UsageProbed`, `Rotated` on the actor relay (emit-on-apply).
//! - An illegal rotate (account onto itself) is rejected by `validate` and audited `Failed`,
//!   emitting nothing.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::json;

use gt_events::Envelope;
use gt_quota::actor;
use gt_quota::{
    Account, AccountQuotaStatus, AccountWindow, ProbeWindow, QuotaCommand, QuotaEvent,
    RotateAccount, SampleTokens, WindowKind,
};
use gt_mcp::{
    audit::{AuditEvent, AuditSink, InMemoryAudit, Outcome},
    auth::Scope,
    McpService,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_drives_quota_sample_probe_rotate_and_emits_events() {
    let (ev_tx, mut ev_rx) = tokio::sync::mpsc::channel::<Envelope<QuotaEvent>>(16);
    let quota = actor::spawn(ev_tx, HashMap::new()); // IDENTITY weights.

    // Seed an account with a live window so sample/probe have something to fold into.
    quota
        .upsert_account(Account {
            id: "acc-1".into(),
            status: AccountQuotaStatus::Healthy,
            window: Some(AccountWindow {
                kind: WindowKind::Rolling5h,
                limit: 1000,
                started_at_secs: 0,
                resets_at_secs: 18_000,
                consumed: 0.0,
            }),
        })
        .await;

    let watcher_audit = Arc::new(InMemoryAudit::new());
    let watcher = McpService::new(
        actor_agent(),
        actor_merge(),
        actor_sched(),
        actor_patrol(),
        actor_orch(),
        quota.clone(),
        Scope::read_only("watcher"),
        Arc::clone(&watcher_audit) as Arc<dyn AuditSink>,
    );

    let admin_audit = Arc::new(InMemoryAudit::new());
    let admin = McpService::new(
        actor_agent(),
        actor_merge(),
        actor_sched(),
        actor_patrol(),
        actor_orch(),
        quota.clone(),
        Scope::admin("admin"),
        Arc::clone(&admin_audit) as Arc<dyn AuditSink>,
    );

    // watcher tries to record a sample → blocked by scope; audited Unauthorized.
    let sample = SampleTokens {
        account: "acc-1".into(),
        session: "sess-a".into(),
        model: "opus".into(),
        input: 100,
        output: 100,
        cache_read: 0,
        cache_creation: 0,
        now_secs: 600,
    };
    let denied = watcher
        .run_quota(
            "quota.sample.execute",
            json!({"account": "acc-1"}),
            QuotaCommand::Sample(sample.clone()),
            false,
        )
        .await;
    let denied_msg = denied
        .expect_err("execute on read_only must fail")
        .message
        .to_string();
    assert!(denied_msg.contains("validate_only"), "denied msg: {denied_msg}");

    // watcher validates a structurally invalid sample (empty account) → Invoked { Failed }.
    let validate_bad = watcher
        .run_quota(
            "quota.sample.validate",
            json!({"account": ""}),
            QuotaCommand::Sample(SampleTokens {
                account: String::new(),
                ..sample.clone()
            }),
            true,
        )
        .await;
    assert!(validate_bad.is_err(), "validate of empty account must fail");

    // admin samples, probes, then rotates. All succeed.
    admin
        .run_quota(
            "quota.sample.execute",
            json!({"account": "acc-1"}),
            QuotaCommand::Sample(sample),
            false,
        )
        .await
        .expect("admin sample.execute should succeed");

    admin
        .run_quota(
            "quota.probe.execute",
            json!({"account": "acc-1"}),
            QuotaCommand::Probe(ProbeWindow {
                account: "acc-1".into(),
                remaining: 250,
                resets_at_secs: 20_000,
                now_secs: 600,
            }),
            false,
        )
        .await
        .expect("admin probe.execute should succeed");

    admin
        .run_quota(
            "quota.rotate.execute",
            json!({"from_account": "acc-1", "to_account": "acc-2"}),
            QuotaCommand::Rotate(RotateAccount {
                from_account: "acc-1".into(),
                to_account: "acc-2".into(),
                now_secs: 700,
            }),
            false,
        )
        .await
        .expect("admin rotate.execute should succeed");

    // Illegal: rotate an account onto itself → rejected by validate.
    let illegal = admin
        .run_quota(
            "quota.rotate.execute",
            json!({"from_account": "acc-2", "to_account": "acc-2"}),
            QuotaCommand::Rotate(RotateAccount {
                from_account: "acc-2".into(),
                to_account: "acc-2".into(),
                now_secs: 800,
            }),
            false,
        )
        .await;
    assert!(illegal.is_err(), "rotating an account onto itself must fail");

    // --- the relay carries exactly TokensSampled, UsageProbed, Rotated (emit-on-apply) ----
    drop(watcher);
    drop(admin);
    drop(quota);
    let mut events = Vec::new();
    while let Some(env) = ev_rx.recv().await {
        events.push(env.payload);
    }
    assert_eq!(
        events,
        vec![
            QuotaEvent::TokensSampled {
                account: "acc-1".into(),
                session: "sess-a".into(),
                model: "opus".into(),
                input: 100,
                output: 100,
                cache_read: 0,
                cache_creation: 0,
                now_secs: 600,
            },
            QuotaEvent::UsageProbed {
                account: "acc-1".into(),
                remaining: 250,
                resets_at_secs: 20_000,
                now_secs: 600,
            },
            QuotaEvent::Rotated {
                from_account: "acc-1".into(),
                to_account: "acc-2".into(),
                now_secs: 700,
            },
        ],
        "only the three accepted commands emit; the illegal rotate emits nothing",
    );

    // --- audit: watcher Unauthorized + failed validate; admin 3 Ok + 1 Failed ------------
    let watcher_events = watcher_audit.snapshot();
    assert!(
        watcher_events.iter().any(|e| matches!(
            e,
            AuditEvent::Unauthorized { tool, .. } if tool == "quota.sample.execute"
        )),
        "watcher audit missing Unauthorized for sample.execute: {watcher_events:?}",
    );
    assert!(
        watcher_events.iter().any(|e| matches!(
            e,
            AuditEvent::Invoked { tool, outcome: Outcome::Failed { .. }, .. } if tool == "quota.sample.validate"
        )),
        "watcher audit missing failed validate: {watcher_events:?}",
    );

    let admin_events = admin_audit.snapshot();
    let ok_count = admin_events
        .iter()
        .filter(|e| matches!(e, AuditEvent::Invoked { outcome: Outcome::Ok, .. }))
        .count();
    assert_eq!(ok_count, 3, "admin: sample+probe+rotate all Ok: {admin_events:?}");
    assert!(
        admin_events.iter().any(|e| matches!(
            e,
            AuditEvent::Invoked { tool, outcome: Outcome::Failed { .. }, .. } if tool == "quota.rotate.execute"
        )),
        "admin audit missing failed illegal rotate.execute: {admin_events:?}",
    );
}

/// Throwaway actors so the service has its full domain surface; this gate exercises only quota.
fn actor_agent() -> gt_agent::actor::AgentHandle {
    gt_agent::actor::spawn(16)
}

fn actor_merge() -> gt_merge::actor::MergeHandle {
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    gt_merge::actor::spawn(tx)
}

fn actor_sched() -> gt_scheduling::actor::SchedHandle {
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    gt_scheduling::actor::spawn(Arc::new(gt_beads::InMemoryBeads::default()), tx, 4)
}

fn actor_patrol() -> gt_patrol::actor::PatrolHandle {
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    gt_patrol::actor::spawn(tx)
}

fn actor_orch() -> gt_orchestration::actor::OrchHandle {
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    gt_orchestration::actor::spawn(tx)
}
