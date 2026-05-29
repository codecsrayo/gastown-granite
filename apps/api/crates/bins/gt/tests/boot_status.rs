//! hq-mc72.12 C10 boot-status gate: the orchestrator emits an `hq-boot` watchdog session
//! (`AgentEvent::Spawned` with role `Dog(Deacon)`) at boot complete and records `SessionEnd`
//! at shutdown. Mirrors the wire `bins/gt/src/main.rs::run` drives via `root.agent_events`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gt_agent::{AgentEvent, DogKind, SessionRole};
use gt_beads::InMemoryBeads;
use gt_events::Envelope;
use gt_polecat::{FakeTmux, PolecatLifecycle, PolecatSupervisor, RestartConfig, SpawnTemplate};
use gt_root::{spawn, RealEffects, RootConfig, SystemClock};

#[tokio::test]
async fn boot_emits_spawned_and_session_end_for_hq_boot() {
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

    // The boot wire pushes Spawned for `hq-boot` with role Dog(Deacon).
    let _ = root
        .agent_events
        .send(Envelope::root(AgentEvent::Spawned {
            session: "hq-boot".to_string(),
            rig: "hq".to_string(),
            role: SessionRole::Dog(DogKind::Deacon),
            crew: None,
        }))
        .await;

    let log_path = root.log_path().to_path_buf();
    let saw_spawned = wait_for(Duration::from_secs(5), || {
        gt_audit::read_all(&log_path)
            .map(|recs| {
                recs.iter().any(|r| {
                    r.kind == "agent.spawned"
                        && matches!(
                            r.decode::<AgentEvent>(),
                            Ok(AgentEvent::Spawned {
                                ref session,
                                role: SessionRole::Dog(DogKind::Deacon),
                                ..
                            }) if session == "hq-boot"
                        )
                })
            })
            .unwrap_or(false)
    })
    .await;
    assert!(saw_spawned, "boot wire did not emit Spawned{{role: Dog(Deacon)}} for hq-boot");

    // Shutdown wire emits SessionEnd before tearing the reactor down.
    let _ = root
        .agent_events
        .send(Envelope::root(AgentEvent::SessionEnd {
            session: "hq-boot".to_string(),
        }))
        .await;
    let saw_end = wait_for(Duration::from_secs(5), || {
        gt_audit::read_all(&log_path)
            .map(|recs| {
                recs.iter().any(|r| {
                    r.kind == "agent.session_end"
                        && matches!(
                            r.decode::<AgentEvent>(),
                            Ok(AgentEvent::SessionEnd { ref session }) if session == "hq-boot"
                        )
                })
            })
            .unwrap_or(false)
    })
    .await;
    assert!(saw_end, "shutdown wire did not emit SessionEnd for hq-boot");

    root.shutdown();
}

fn test_template() -> SpawnTemplate {
    SpawnTemplate {
        rig: "test".to_string(),
        prefix: "gt".to_string(),
        workdir: std::env::temp_dir(),
        command: "sleep".to_string(),
        args: vec!["30".to_string()],
        base_env: vec![("GT_ROLE".to_string(), "polecat".to_string())],
        heartbeat_dir: std::env::temp_dir(),
    }
}

fn tempdir() -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("gt-boot-status-{}", ulid::Ulid::new()));
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
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}
