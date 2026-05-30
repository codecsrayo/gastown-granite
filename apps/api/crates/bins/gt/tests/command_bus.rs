//! Gate for hq-fe-api-w.1: the `CommandBus` exposed by `RootHandle::commands()` routes
//! every domain command through the same dispatcher.
//!
//! Verifies the bus dispatches happy-path commands to all seven domains and surfaces
//! `"rig domain not wired"` when the bus is built without a rig handle.
//!
//! Test uses the real `spawn` composition root over in-memory repos so the bus drives the
//! same actors the gt-rs binary drives at runtime — no fakes besides the bead/repo stubs.

use std::path::PathBuf;
use std::sync::Arc;

use gt_agent::{AddSession, AgentCommand};
use gt_beads::InMemoryBeads;
use gt_merge::{MergeCommand, SubmitMerge};
use gt_orchestration::{LaunchConvoy, OrchCommand};
use gt_patrol::{PatrolCommand, RegisterLease};
use gt_polecat::{FakeTmux, PolecatLifecycle, PolecatSupervisor, RestartConfig, SpawnTemplate};
use gt_quota::{QuotaCommand, SampleTokens};
use gt_root::{spawn, CommandBus, RealEffects, RootCommand, RootConfig, SystemClock};
use gt_scheduling::{Enqueue, SchedCommand};

fn test_template() -> SpawnTemplate {
    SpawnTemplate {
        prefix: "gt".to_string(),
        rig: "granite".to_string(),
        workdir: std::env::temp_dir(),
        command: "sleep".to_string(),
        args: vec!["30".to_string()],
        base_env: vec![("GT_ROLE".to_string(), "polecat".to_string())],
        heartbeat_dir: std::env::temp_dir(),
    }
}

fn tempdir() -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("gt-cmdbus-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn fresh_root_with_bus() -> (
    gt_root::RootHandle<Arc<InMemoryBeads>>,
    CommandBus,
    PathBuf,
) {
    let dir = tempdir();
    let log = dir.join("events.jsonl");
    let lifecycle = PolecatLifecycle::new(Box::new(FakeTmux::new()), test_template());
    let supervisor = Arc::new(PolecatSupervisor::new(
        Arc::new(FakeTmux::new()),
        RestartConfig::default(),
        u32::MAX,
    ));
    let (effects, quota_slot) = RealEffects::new(lifecycle, supervisor);
    let root = spawn(
        Arc::new(InMemoryBeads::default()),
        Arc::new(gt_merge::InMemoryMergeRepo::default()),
        Arc::new(gt_patrol::InMemoryPatrolRepo::default()),
        Arc::new(gt_orchestration::InMemoryOrchRepo::default()),
        effects,
        SystemClock,
        &log,
        RootConfig::default(),
    );
    let _ = quota_slot.set(root.quota.clone());
    let bus = root.commands();
    (root, bus, dir)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bus_dispatches_every_domain() {
    let (_root, bus, _dir) = fresh_root_with_bus();

    // Agent — happy path. Add a session.
    bus.dispatch(
        RootCommand::Agent(AgentCommand::Add(AddSession {
            id: "p1".into(),
            rig: "granite".into(),
        })),
        None,
    )
    .await
    .expect("agent.add dispatches");

    // Scheduling — happy path. Enqueue a bead.
    bus.dispatch(
        RootCommand::Sched(SchedCommand::Enqueue(Enqueue {
            bead: "hq-1".into(),
            priority: 1,
        })),
        None,
    )
    .await
    .expect("scheduling.enqueue dispatches");

    // Patrol — happy path. Open a lease.
    bus.dispatch(
        RootCommand::Patrol(PatrolCommand::Register(RegisterLease {
            bead: "hq-1".into(),
            worker: "p1".into(),
            priority: 1,
            now_secs: 0,
        })),
        None,
    )
    .await
    .expect("patrol.register dispatches");

    // Merge — happy path. Submit a slot.
    bus.dispatch(
        RootCommand::Merge(MergeCommand::Submit(SubmitMerge {
            bead: "hq-1".into(),
            branch: "feat/hq-1".into(),
            channel_msg_id: "evt-1".into(),
        })),
        None,
    )
    .await
    .expect("merge.submit dispatches");

    // Orch — happy path. Launch a convoy with a single member.
    bus.dispatch(
        RootCommand::Orch(OrchCommand::Launch(LaunchConvoy {
            convoy: "c1".into(),
            members: vec!["hq-1".into()],
        })),
        None,
    )
    .await
    .expect("orch.launch dispatches");

    // Quota — happy path. Sample is a shape-only validate; non-empty fields pass.
    // The point is the bus routed it to the quota actor (a wrong route would panic
    // on `unwrap()` inside the actor mailbox match).
    bus.validate(
        &RootCommand::Quota(QuotaCommand::Sample(SampleTokens {
            account: "demo-account".into(),
            session: "p1".into(),
            model: "claude-opus".into(),
            input: 1,
            output: 0,
            cache_read: 0,
            cache_creation: 0,
            now_secs: 0,
        })),
        None,
    )
    .await
    .expect("quota.sample validate dispatches");

    // Rig — wired by `spawn`. `validate` with an empty name hits the rig actor and
    // returns its grammar error; the assertion proves the bus routed to the rig path
    // (rather than the unwired branch).
    let rig_result = bus
        .validate(
            &RootCommand::Rig(gt_rig::RigCommand::Add(gt_rig::AddRig {
                name: String::new(),
                prefix: String::new(),
                git_url: String::new(),
                push_url: None,
                upstream_url: None,
                default_branch: String::new(),
                now_secs: 0,
            })),
            None,
        )
        .await;
    let rig_err = rig_result.expect_err("empty AddRig is invalid");
    assert!(
        !rig_err.to_string().contains("rig domain not wired"),
        "bus must reach the rig actor, not the not-wired branch",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bus_without_rig_returns_not_wired() {
    // Build a bus that intentionally drops the rig handle.
    let (_root, bus, _dir) = fresh_root_with_bus();
    let bus_no_rig = CommandBus::new(
        bus.agent().clone(),
        bus.merge().clone(),
        bus.sched().clone(),
        bus.patrol().clone(),
        bus.orch().clone(),
        bus.quota().clone(),
    );
    let err = bus_no_rig
        .dispatch(
            RootCommand::Rig(gt_rig::RigCommand::Add(gt_rig::AddRig {
                name: "demo".into(),
                prefix: "demo".into(),
                git_url: "git@example.com:demo/demo.git".into(),
                push_url: None,
                upstream_url: None,
                default_branch: "main".into(),
                now_secs: 0,
            })),
            None,
        )
        .await
        .expect_err("dispatch should fail without rig handle");
    assert!(
        err.to_string().contains("rig domain not wired"),
        "expected `rig domain not wired`, got `{err}`",
    );
}
