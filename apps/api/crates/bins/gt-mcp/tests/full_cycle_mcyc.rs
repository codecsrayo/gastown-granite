//! hq-mcyc.3 gate: drive the entire MCP tool surface in one pass against a live composition
//! root and assert the cross-domain state machines all return to a clean baseline.
//!
//! The MCP full-cycle test session that motivated the `hq-mcyc` epic exercised 22 tools by
//! hand (the 11 validate + 11 execute pairs across 7 actors, plus the agent lifecycle and
//! patrol stale detector) and surfaced two real bugs the dispatcher had been hiding:
//! a capacity leak when the bead repo errored (hq-mcyc.2), and a sibling leak when a merge
//! failed (hq-mcyc.6). This test reruns that same sequence end-to-end so any future
//! regression on either leak — or the reactor's Ready -> Merging auto-advance, or the orch
//! convoy handoff — is caught in CI instead of in a 20-step manual repro.
//!
//! Scope: in-memory adapters only; the root spawns its real reactor loop so cross-domain
//! reactions (`MergeEvent::Merged → sched.capacity_freed`, `LeaseExpired → cas_release +
//! enqueue`, `OrchEvent::MemberDispatched → effects.sling`) all fire.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;

use gt_agent::{AddSession, AgentCommand, RemoveSession, SessionState, TransitionSession};
use gt_beads::{Bead, BeadRepository, BeadStatus, InMemoryBeads};
use gt_mcp::{
    audit::{AuditSink, InMemoryAudit},
    auth::Scope,
    CreateBead, McpService, RegisterAccount, SessionsRead,
};
use gt_merge::{CompleteMerge, MergeCommand, SubmitMerge};
use gt_orchestration::{CompleteMember, LaunchConvoy, OrchCommand};
use gt_patrol::{CloseLease, Heartbeat, PatrolCommand, RegisterLease, Tick};
use gt_quota::{ProbeWindow, QuotaCommand, RotateAccount, SampleTokens};
use gt_root::{spawn, LogEffects, RootConfig, SystemClock};
use gt_scheduling::{Enqueue, MarkDispatched, SchedCommand};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mcp_full_cycle_returns_to_clean_baseline() {
    let dir = tempdir();
    let log = dir.join("events.jsonl");

    // Real root + reactor. capacity = 1 so the dispatcher slot release after merge.complete
    // is observable in the snapshot. LogEffects is enough: the assertions don't depend on a
    // real polecat sling/rotate adapter, only on the reactor's in-process arms.
    let beads = Arc::new(InMemoryBeads::default());
    beads
        .upsert(&Bead::new(
            "hq-mcyc-3-b1",
            "full-cycle test bead",
            BeadStatus::Pending,
            1,
        ))
        .await
        .unwrap();
    beads
        .upsert(&Bead::new(
            "hq-mcyc-3-b2",
            "convoy member 1",
            BeadStatus::Pending,
            1,
        ))
        .await
        .unwrap();
    beads
        .upsert(&Bead::new(
            "hq-mcyc-3-b3",
            "convoy member 2",
            BeadStatus::Pending,
            1,
        ))
        .await
        .unwrap();

    let root = spawn(
        beads.clone(),
        Arc::new(gt_merge::InMemoryMergeRepo::default()),
        Arc::new(gt_patrol::InMemoryPatrolRepo::default()),
        Arc::new(gt_orchestration::InMemoryOrchRepo::default()),
        LogEffects,
        SystemClock,
        &log,
        RootConfig {
            capacity: 1,
            ..RootConfig::default()
        },
    );

    // MCP service over the same actor handles the root drives — exactly how `bins/gt-mcp::main`
    // wires production.
    let audit = Arc::new(InMemoryAudit::new());
    let svc = McpService::with_sessions(
        root.agent.clone(),
        SessionsRead::Actor(root.agent.clone()),
        root.merge.clone(),
        root.sched.clone(),
        root.patrol.clone(),
        root.orch.clone(),
        root.quota.clone(),
        Scope::admin("mcyc-3"),
        audit as Arc<dyn AuditSink>,
        Some(root.agent_events.clone()),
    );

    let t0: u64 = 1_780_000_000;

    // 1. agent.add — registers a polecat session.
    svc.run(
        "agent.add.execute",
        json!({"id": "mcyc3-s1", "rig": "mcyc3"}),
        AgentCommand::Add(AddSession {
            id: "mcyc3-s1".into(),
            rig: "mcyc3".into(),
        }),
        false,
    )
    .await
    .expect("agent.add");

    // 2. scheduling.create_bead — already populated above; call run_create_bead to exercise
    //    the MCP path as well (idempotent upsert).
    svc.run_create_bead(
        "scheduling.create_bead.execute",
        CreateBead {
            id: "hq-mcyc-3-b1".into(),
            title: "full-cycle test bead".into(),
            priority: 1,
        },
        false,
    )
    .await
    .expect("create_bead");

    // 3-4. scheduling.enqueue + scheduling.mark_dispatched — consume the only slot.
    svc.run_sched(
        "scheduling.enqueue.execute",
        json!({"bead": "hq-mcyc-3-b1", "priority": 1}),
        SchedCommand::Enqueue(Enqueue {
            bead: "hq-mcyc-3-b1".into(),
            priority: 1,
        }),
        false,
    )
    .await
    .expect("enqueue");

    // Wait for the dispatcher pump to claim + reactor to register the lease, then
    // additionally mark a worker so the test exercises the manual dispatch path too.
    wait_for(Duration::from_secs(3), || {
        any_kind(root.log_path(), "scheduling.dispatched")
    })
    .await;

    // 5. agent.transition Spawned -> Working (the bead is in flight now).
    svc.run(
        "agent.transition.execute",
        json!({"id": "mcyc3-s1", "to": "working"}),
        AgentCommand::Transition(TransitionSession {
            id: "mcyc3-s1".into(),
            to: SessionState::Working,
        }),
        false,
    )
    .await
    .expect("agent.transition working");

    // 6-7. patrol.register + patrol.heartbeat (the reactor already registered on dispatch;
    // these calls are idempotent for the same lease and exercise the MCP path).
    svc.run_patrol(
        "patrol.register.execute",
        json!({"bead": "hq-mcyc-3-b1", "worker": "mcyc3-w1", "priority": 1, "now_secs": t0}),
        PatrolCommand::Register(RegisterLease {
            bead: "hq-mcyc-3-b1".into(),
            worker: "mcyc3-w1".into(),
            priority: 1,
            now_secs: t0,
        }),
        false,
    )
    .await
    .expect("patrol.register");

    svc.run_patrol(
        "patrol.heartbeat.execute",
        json!({"worker": "mcyc3-w1", "now_secs": t0 + 5}),
        PatrolCommand::Heartbeat(Heartbeat {
            worker: "mcyc3-w1".into(),
            now_secs: t0 + 5,
        }),
        false,
    )
    .await
    .expect("patrol.heartbeat");

    // 8. quota.register — two accounts so rotate has a target.
    svc.run_quota_register(
        "quota.register.execute",
        RegisterAccount {
            account: "mcyc3-acct-a".into(),
            limit: 100_000,
            started_at_secs: t0,
            resets_at_secs: t0 + 18_000,
            weekly: false,
        },
        false,
    )
    .await
    .expect("quota.register a");
    svc.run_quota_register(
        "quota.register.execute",
        RegisterAccount {
            account: "mcyc3-acct-b".into(),
            limit: 100_000,
            started_at_secs: t0,
            resets_at_secs: t0 + 18_000,
            weekly: false,
        },
        false,
    )
    .await
    .expect("quota.register b");

    // 9-10. quota.sample + quota.probe — feed the EWMA + reconcile against synthetic headers.
    svc.run_quota(
        "quota.sample.execute",
        json!({
            "account": "mcyc3-acct-a",
            "session": "mcyc3-s1",
            "model": "claude-opus-4-7",
            "input": 1200, "output": 800, "cache_read": 500, "cache_creation": 100,
            "now_secs": t0 + 20
        }),
        QuotaCommand::Sample(SampleTokens {
            account: "mcyc3-acct-a".into(),
            session: "mcyc3-s1".into(),
            model: "claude-opus-4-7".into(),
            input: 1200,
            output: 800,
            cache_read: 500,
            cache_creation: 100,
            now_secs: t0 + 20,
        }),
        false,
    )
    .await
    .expect("quota.sample");
    svc.run_quota(
        "quota.probe.execute",
        json!({
            "account": "mcyc3-acct-a", "remaining": 87500,
            "resets_at_secs": t0 + 18_000, "now_secs": t0 + 25
        }),
        QuotaCommand::Probe(ProbeWindow {
            account: "mcyc3-acct-a".into(),
            remaining: 87_500,
            resets_at_secs: t0 + 18_000,
            now_secs: t0 + 25,
        }),
        false,
    )
    .await
    .expect("quota.probe");

    // 11-12. merge.submit + merge.complete — the reactor auto-advances Ready -> Merging,
    // and Merged triggers `sched.capacity_freed` (hq-mcyc.2).
    svc.run_merge(
        "merge.submit.execute",
        json!({"bead": "hq-mcyc-3-b1", "branch": "feat/mcyc-3", "channel_msg_id": "01J0MCYC3EVT01"}),
        MergeCommand::Submit(SubmitMerge {
            bead: "hq-mcyc-3-b1".into(),
            branch: "feat/mcyc-3".into(),
            channel_msg_id: "01J0MCYC3EVT01".into(),
        }),
        false,
    )
    .await
    .expect("merge.submit");

    wait_for(Duration::from_secs(3), || {
        any_kind(root.log_path(), "merge.started")
    })
    .await;

    svc.run_merge(
        "merge.complete.execute",
        json!({"bead": "hq-mcyc-3-b1", "sha": "deadbeefcafebabe1234567890abcdef00000003"}),
        MergeCommand::Complete(CompleteMerge {
            bead: "hq-mcyc-3-b1".into(),
            sha: "deadbeefcafebabe1234567890abcdef00000003".into(),
        }),
        false,
    )
    .await
    .expect("merge.complete");

    // 13. patrol.close — release the lease cleanly so the next tick is a no-op (hq-mcyc.4).
    svc.run_patrol(
        "patrol.close.execute",
        json!({"bead": "hq-mcyc-3-b1"}),
        PatrolCommand::Close(CloseLease { bead: "hq-mcyc-3-b1".into() }),
        false,
    )
    .await
    .expect("patrol.close");

    // 14. patrol.tick well past the timeout — should be a no-op because the lease was closed.
    svc.run_patrol(
        "patrol.tick.execute",
        json!({"now_secs": t0 + 99_999, "timeout_secs": 60}),
        PatrolCommand::Tick(Tick {
            now_secs: t0 + 99_999,
            timeout_secs: 60,
        }),
        false,
    )
    .await
    .expect("patrol.tick");

    // 15-17. orch.launch_convoy + 2x complete_member — the convoy closes on the second.
    svc.run_orch(
        "orch.launch_convoy.execute",
        json!({"convoy": "mcyc3-cv1", "members": ["hq-mcyc-3-b2", "hq-mcyc-3-b3"]}),
        OrchCommand::Launch(LaunchConvoy {
            convoy: "mcyc3-cv1".into(),
            members: vec!["hq-mcyc-3-b2".into(), "hq-mcyc-3-b3".into()],
        }),
        false,
    )
    .await
    .expect("orch.launch_convoy");
    svc.run_orch(
        "orch.complete_member.execute",
        json!({"convoy": "mcyc3-cv1", "member": "hq-mcyc-3-b2"}),
        OrchCommand::Complete(CompleteMember {
            convoy: "mcyc3-cv1".into(),
            member: "hq-mcyc-3-b2".into(),
        }),
        false,
    )
    .await
    .expect("orch.complete_member 1");
    svc.run_orch(
        "orch.complete_member.execute",
        json!({"convoy": "mcyc3-cv1", "member": "hq-mcyc-3-b3"}),
        OrchCommand::Complete(CompleteMember {
            convoy: "mcyc3-cv1".into(),
            member: "hq-mcyc-3-b3".into(),
        }),
        false,
    )
    .await
    .expect("orch.complete_member 2");

    // 18. quota.rotate — park account a, swap to b. Domain emits Rotated; reactor flips the
    // keychain pointer.
    svc.run_quota(
        "quota.rotate.execute",
        json!({"from_account": "mcyc3-acct-a", "to_account": "mcyc3-acct-b", "now_secs": t0 + 30}),
        QuotaCommand::Rotate(RotateAccount {
            from_account: "mcyc3-acct-a".into(),
            to_account: "mcyc3-acct-b".into(),
            now_secs: t0 + 30,
        }),
        false,
    )
    .await
    .expect("quota.rotate");

    // 19. agent.transition Working -> Done.
    svc.run(
        "agent.transition.execute",
        json!({"id": "mcyc3-s1", "to": "done"}),
        AgentCommand::Transition(TransitionSession {
            id: "mcyc3-s1".into(),
            to: SessionState::Done,
        }),
        false,
    )
    .await
    .expect("agent.transition done");

    // 20. agent.remove — session leaves the registry.
    svc.run(
        "agent.remove.execute",
        json!({"id": "mcyc3-s1"}),
        AgentCommand::Remove(RemoveSession { id: "mcyc3-s1".into() }),
        false,
    )
    .await
    .expect("agent.remove");

    // Exercise the manual dispatch path one more time so the regression test would also catch
    // a `mark_dispatched` capacity leak — submit a fresh bead through the same merge path.
    svc.run_create_bead(
        "scheduling.create_bead.execute",
        CreateBead {
            id: "hq-mcyc-3-b4".into(),
            title: "manual mark_dispatched probe".into(),
            priority: 1,
        },
        false,
    )
    .await
    .expect("create_bead b4");
    // Wait for the previous slot to free before reusing the capacity.
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let (_, n) = root.sched.snapshot().await;
        if n == 0 {
            break;
        }
        if Instant::now() >= deadline {
            panic!("capacity not released after first merge.complete — hq-mcyc.2 regression");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    svc.run_sched(
        "scheduling.mark_dispatched.execute",
        json!({"bead": "hq-mcyc-3-b4", "worker": "mcyc3-w2"}),
        SchedCommand::MarkDispatched(MarkDispatched {
            bead: "hq-mcyc-3-b4".into(),
            worker: "mcyc3-w2".into(),
        }),
        false,
    )
    .await
    .expect("mark_dispatched");
    svc.run_merge(
        "merge.submit.execute",
        json!({"bead": "hq-mcyc-3-b4", "branch": "feat/mcyc-3-b4", "channel_msg_id": "01J0MCYC3EVT02"}),
        MergeCommand::Submit(SubmitMerge {
            bead: "hq-mcyc-3-b4".into(),
            branch: "feat/mcyc-3-b4".into(),
            channel_msg_id: "01J0MCYC3EVT02".into(),
        }),
        false,
    )
    .await
    .expect("merge.submit b4");
    wait_for(Duration::from_secs(3), || {
        // wait until merge slot is in merging state from reactor auto-advance
        any_kind_count(root.log_path(), "merge.started") >= 2
    })
    .await;
    svc.run_merge(
        "merge.complete.execute",
        json!({"bead": "hq-mcyc-3-b4", "sha": "deadbeefcafebabe1234567890abcdef00000004"}),
        MergeCommand::Complete(CompleteMerge {
            bead: "hq-mcyc-3-b4".into(),
            sha: "deadbeefcafebabe1234567890abcdef00000004".into(),
        }),
        false,
    )
    .await
    .expect("merge.complete b4");

    // Close the lease the reactor auto-opened on the mark_dispatched above so the final
    // snapshot lands at zero live leases (the b1 lease was closed explicitly earlier).
    svc.run_patrol(
        "patrol.close.execute",
        json!({"bead": "hq-mcyc-3-b4"}),
        PatrolCommand::Close(CloseLease { bead: "hq-mcyc-3-b4".into() }),
        false,
    )
    .await
    .expect("patrol.close b4");

    // --- assertions ----------------------------------------------------------------------

    // Sessions: removed.
    let sessions = root.agent.snapshot().await;
    assert!(
        sessions.is_empty(),
        "agent registry must be empty after remove, got: {sessions:?}"
    );

    // Scheduler: in_flight back to 0 (covers .2 + .6 invariants).
    let deadline = Instant::now() + Duration::from_secs(3);
    let final_in_flight = loop {
        let (_, n) = root.sched.snapshot().await;
        if n == 0 || Instant::now() >= deadline {
            break n;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(
        final_in_flight, 0,
        "in_flight stranded after full cycle (regression on hq-mcyc.2 or .6)"
    );

    // Patrol: no live leases, exactly the heartbeat-window we closed must have left no
    // expirations (tick was a no-op because the lease was closed first — hq-mcyc.4 invariant).
    let (live, expired) = root.patrol.snapshot().await;
    assert_eq!(live, 0, "patrol leases must drain");
    assert_eq!(
        expired, 0,
        "tick after clean close must not emit LeaseExpired"
    );

    // Merge slots: both beads landed in `merged`.
    let merge_snapshot = root.merge.snapshot().await;
    let merged: Vec<&str> = merge_snapshot
        .iter()
        .filter(|s| matches!(s.state, gt_merge::MergeSlotState::Merged))
        .map(|s| s.bead.as_str())
        .collect();
    assert!(
        merged.contains(&"hq-mcyc-3-b1") && merged.contains(&"hq-mcyc-3-b4"),
        "both merges should be Merged, got: {merge_snapshot:?}"
    );

    // Orch: convoy closed.
    let convoys = root.orch.snapshot().await;
    let cv = convoys
        .iter()
        .find(|c| c.id == "mcyc3-cv1")
        .expect("convoy must exist");
    assert!(
        matches!(cv.state, gt_orchestration::ConvoyState::Closed),
        "convoy must be Closed, got: {:?}",
        cv.state
    );

    // Quota: two accounts registered, rotation event landed in the log.
    let (accounts, _predictions) = root.quota.snapshot().await;
    assert_eq!(accounts, 2);
    assert!(
        any_kind(root.log_path(), "quota.rotated"),
        "quota.rotate must emit quota.rotated"
    );

    root.shutdown();
}

fn any_kind(path: &std::path::Path, kind: &str) -> bool {
    gt_audit::read_all(path)
        .map(|recs| recs.iter().any(|r| r.kind == kind))
        .unwrap_or(false)
}

fn any_kind_count(path: &std::path::Path, kind: &str) -> usize {
    gt_audit::read_all(path)
        .map(|recs| recs.iter().filter(|r| r.kind == kind).count())
        .unwrap_or(0)
}

fn tempdir() -> PathBuf {
    let mut p = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    p.push(format!("gt-mcyc-3-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

async fn wait_for(timeout: Duration, mut pred: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if pred() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
