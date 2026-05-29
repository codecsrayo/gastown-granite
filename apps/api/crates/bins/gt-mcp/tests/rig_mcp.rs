//! Gate test for hq-mc72.12.29 (rig MCP wire): gt-mcp drives the rig catalog domain
//! end-to-end through the `Command { validate, execute }` path.
//!
//! `McpService::run_rig` is the shared dispatch backing every `rig.*` `#[tool]` method.
//! Driving it directly covers the same scope + audit + actor path the wire transport hits.
//!
//! Covers:
//! - A full-scope add/set_prefix/remove mutates the shared catalog and emits exactly
//!   `Added`, `PrefixChanged`, `Removed` on the actor relay (emit-on-apply).
//! - A `read_only` scope rejects `rig.add.execute` and records `Unauthorized`.
//! - A duplicate add is rejected by the actor's revalidation, audited `Invoked { Failed }`,
//!   emitting nothing.
//! - When the service has no `RigHandle` (the `main.rs` default until hq-mc72.12.30 wires
//!   the composition root), `rig.add.execute` returns `rig domain not wired` and is audited
//!   `Failed` — the wire surface is live even before the actor injection.
//! - The `gt://rigs` resource returns the catalog snapshot (and an empty array when unwired).

use std::sync::Arc;

use serde_json::json;

use gt_events::Envelope;
use gt_rig::actor;
use gt_rig::{AddRig, RemoveRig, RigCommand, RigEvent, SetRigPrefix};

use gt_mcp::{
    audit::{AuditEvent, AuditSink, InMemoryAudit, Outcome},
    auth::Scope,
    McpService,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_drives_rig_add_set_prefix_remove_and_emits_events() {
    let (ev_tx, mut ev_rx) = tokio::sync::mpsc::channel::<Envelope<RigEvent>>(16);
    let rig = actor::spawn(ev_tx);

    let admin_audit = Arc::new(InMemoryAudit::new());
    let admin = McpService::new(
        actor_agent(),
        actor_merge(),
        actor_sched(),
        actor_patrol(),
        actor_orch(),
        actor_quota(),
        Scope::admin("admin"),
        Arc::clone(&admin_audit) as Arc<dyn AuditSink>,
    )
    .with_rig(rig.clone());

    // add
    let add = AddRig {
        name: "plane".into(),
        prefix: "pl".into(),
        git_url: "git@github.com:o/plane.git".into(),
        push_url: None,
        upstream_url: None,
        default_branch: "main".into(),
        now_secs: 100,
    };
    admin
        .run_rig(
            "rig.add.execute",
            json!(add),
            RigCommand::Add(add.clone()),
            false,
        )
        .await
        .expect("add ok");
    let ev = ev_rx.recv().await.expect("Added emitted");
    assert!(matches!(ev.payload, RigEvent::Added { .. }));

    // set_prefix
    let setp = SetRigPrefix {
        name: "plane".into(),
        new_prefix: "pln".into(),
        now_secs: 101,
    };
    admin
        .run_rig(
            "rig.set_prefix.execute",
            json!(setp),
            RigCommand::SetPrefix(setp),
            false,
        )
        .await
        .expect("set_prefix ok");
    let ev = ev_rx.recv().await.expect("PrefixChanged emitted");
    assert!(matches!(ev.payload, RigEvent::PrefixChanged { .. }));

    // gt://rigs reflects the live catalog
    let snap = admin.read_resource_json("gt://rigs").await.unwrap();
    let arr = snap.as_array().expect("rigs is an array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["prefix"], "pln");

    // remove
    let rm = RemoveRig {
        name: "plane".into(),
        now_secs: 102,
    };
    admin
        .run_rig(
            "rig.remove.execute",
            json!(rm),
            RigCommand::Remove(rm),
            false,
        )
        .await
        .expect("remove ok");
    let ev = ev_rx.recv().await.expect("Removed emitted");
    assert!(matches!(ev.payload, RigEvent::Removed { .. }));

    assert_eq!(rig.snapshot().await, 0);

    // Three successful invocations were audited Ok.
    let oks = admin_audit
        .snapshot()
        .iter()
        .filter(|e| matches!(e, AuditEvent::Invoked { outcome: Outcome::Ok, .. }))
        .count();
    assert_eq!(oks, 3, "add + set_prefix + remove audited Ok");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_only_scope_rejects_rig_add_and_audits_unauthorized() {
    let (ev_tx, _ev_rx) = tokio::sync::mpsc::channel::<Envelope<RigEvent>>(16);
    let rig = actor::spawn(ev_tx);

    let audit = Arc::new(InMemoryAudit::new());
    let watcher = McpService::new(
        actor_agent(),
        actor_merge(),
        actor_sched(),
        actor_patrol(),
        actor_orch(),
        actor_quota(),
        Scope::read_only("watcher"),
        Arc::clone(&audit) as Arc<dyn AuditSink>,
    )
    .with_rig(rig);

    let add = AddRig {
        name: "plane".into(),
        prefix: "pl".into(),
        git_url: "git@x:y/plane.git".into(),
        push_url: None,
        upstream_url: None,
        default_branch: "main".into(),
        now_secs: 1,
    };
    let res = watcher
        .run_rig("rig.add.execute", json!(add), RigCommand::Add(add), false)
        .await;
    assert!(res.is_err(), "read_only must reject rig.add.execute");
    assert!(audit
        .snapshot()
        .iter()
        .any(|e| matches!(e, AuditEvent::Unauthorized { .. })));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_add_is_revalidated_and_emits_nothing() {
    let (ev_tx, mut ev_rx) = tokio::sync::mpsc::channel::<Envelope<RigEvent>>(16);
    let rig = actor::spawn(ev_tx);

    let audit = Arc::new(InMemoryAudit::new());
    let admin = McpService::new(
        actor_agent(),
        actor_merge(),
        actor_sched(),
        actor_patrol(),
        actor_orch(),
        actor_quota(),
        Scope::admin("admin"),
        Arc::clone(&audit) as Arc<dyn AuditSink>,
    )
    .with_rig(rig);

    let add = AddRig {
        name: "plane".into(),
        prefix: "pl".into(),
        git_url: "git@x:y/plane.git".into(),
        push_url: None,
        upstream_url: None,
        default_branch: "main".into(),
        now_secs: 1,
    };
    admin
        .run_rig(
            "rig.add.execute",
            json!(add),
            RigCommand::Add(add.clone()),
            false,
        )
        .await
        .unwrap();
    let _ = ev_rx.recv().await.expect("first Added");

    // Second add with the same name must fail the actor's revalidation.
    let dup = AddRig {
        prefix: "px".into(),
        ..add
    };
    let res = admin
        .run_rig("rig.add.execute", json!(dup), RigCommand::Add(dup), false)
        .await;
    assert!(res.is_err(), "duplicate name rejected");
    assert!(ev_rx.try_recv().is_err(), "no event on failed exec");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unwired_rig_returns_not_wired_and_empty_resource() {
    let audit = Arc::new(InMemoryAudit::new());
    // No `.with_rig(...)` — the `main.rs` default until hq-mc72.12.30.
    let admin = McpService::new(
        actor_agent(),
        actor_merge(),
        actor_sched(),
        actor_patrol(),
        actor_orch(),
        actor_quota(),
        Scope::admin("admin"),
        Arc::clone(&audit) as Arc<dyn AuditSink>,
    );

    let add = AddRig {
        name: "plane".into(),
        prefix: "pl".into(),
        git_url: "git@x:y/plane.git".into(),
        push_url: None,
        upstream_url: None,
        default_branch: "main".into(),
        now_secs: 1,
    };
    let res = admin
        .run_rig("rig.add.execute", json!(add), RigCommand::Add(add), false)
        .await;
    assert!(res.is_err(), "unwired rig must fail the tool");
    assert!(audit.snapshot().iter().any(|e| matches!(
        e,
        AuditEvent::Invoked { outcome: Outcome::Failed { .. }, .. }
    )));

    let snap = admin.read_resource_json("gt://rigs").await.unwrap();
    assert_eq!(snap.as_array().map(|a| a.len()), Some(0));
}

fn actor_agent() -> gt_agent::actor::AgentHandle {
    gt_agent::actor::spawn(16)
}
fn actor_merge() -> gt_merge::actor::MergeHandle {
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    gt_merge::actor::spawn(gt_merge::InMemoryMergeRepo::default(), tx)
}
fn actor_sched() -> gt_scheduling::actor::SchedHandle {
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    gt_scheduling::actor::spawn(Arc::new(gt_beads::InMemoryBeads::default()), tx, 4)
}
fn actor_patrol() -> gt_patrol::actor::PatrolHandle {
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    gt_patrol::actor::spawn(gt_patrol::InMemoryPatrolRepo::default(), tx)
}
fn actor_orch() -> gt_orchestration::actor::OrchHandle {
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    gt_orchestration::actor::spawn(gt_orchestration::InMemoryOrchRepo::default(), tx)
}
fn actor_quota() -> gt_quota::actor::QuotaHandle {
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    gt_quota::actor::spawn(tx, std::collections::HashMap::new())
}
