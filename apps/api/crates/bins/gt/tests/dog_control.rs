//! hq-mc72.12 C10 gate: the dog-control channel watcher spawns/stops named town dog sessions.
//! An operator `spawn` message creates an `hq-dog-<name>` tmux session and the dog is tracked
//! as a `role=dog` session (agent.spawned) in the audit log; `stop` kills it and emits
//! agent.session_end. Mirrors `bins/gt/src/main.rs::spawn_dog_daemon`. Real tmux on a private
//! `-L` socket; skips when tmux is not installed.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gt_agent::{AgentEvent, DogKind, SessionRole};
use gt_beads::InMemoryBeads;
use gt_channel::Channel;
use gt_events::Envelope;
use gt_polecat::{
    spawn_tmux, FakeTmux, PolecatLifecycle, PolecatSupervisor, RestartConfig, SpawnSpec,
    SpawnTemplate, Tmux, TmuxCli,
};
use gt_root::{spawn, RealEffects, RootConfig, SystemClock};

#[derive(serde::Deserialize)]
struct DogReq {
    name: String,
    action: String,
}

#[tokio::test]
async fn dog_spawn_and_stop_tracked_via_agent_events() {
    if !which_tmux() {
        eprintln!("skipping: tmux not on PATH");
        return;
    }
    let dir = tempdir();
    let log = dir.join("events.jsonl");
    let socket = format!("gt-dog-test-{}", ulid::Ulid::new());
    let channel = Channel::open(&dir, "dog").expect("open dog channel");

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

    // Replicate spawn_dog_daemon's handler against a private tmux socket.
    let agent_events = root.agent_events.clone();
    let chan = channel.clone();
    let socket_for_task = socket.clone();
    let task = tokio::spawn(async move {
        let tmux = TmuxCli::new().with_socket(&socket_for_task);
        let mut rx = chan.subscribe(64).unwrap();
        while let Some(msg) = rx.recv().await {
            if let Ok(req) = serde_json::from_slice::<DogReq>(&msg.payload) {
                let session = format!("hq-dog-{}", req.name);
                if req.action == "spawn" {
                    let spec = SpawnSpec {
                        session: session.clone(),
                        rig: "hq".to_string(),
                        polecat: req.name.clone(),
                        crew: None,
                        workdir: std::env::temp_dir(),
                        command: "sleep".to_string(),
                        args: vec!["30".to_string()],
                        env: vec![("GT_ROLE".to_string(), "dog".to_string())],
                        hook_bead: None,
                        issue: None,
                        heartbeat: std::env::temp_dir().join(format!("{session}.hb")),
                    };
                    spawn_tmux(&tmux, &spec).unwrap();
                    let _ = agent_events
                        .send(Envelope::root(AgentEvent::Spawned {
                            session: session.clone(),
                            rig: "hq".to_string(),
                            role: SessionRole::Dog(DogKind::Dog),
                            crew: None,
                        }))
                        .await;
                } else {
                    let _ = tmux.kill_session(&session);
                    let _ = agent_events
                        .send(Envelope::root(AgentEvent::SessionEnd {
                            session: session.clone(),
                        }))
                        .await;
                }
            }
            let _ = chan.ack(&msg);
        }
    });

    let probe = TmuxCli::new().with_socket(&socket);
    let log_path = root.log_path().to_path_buf();

    // --- spawn ---
    channel
        .emit(&serde_json::to_vec(&serde_json::json!({"name":"rex","action":"spawn"})).unwrap())
        .unwrap();
    let spawned = wait_for(Duration::from_secs(5), || probe.has_session("hq-dog-rex")).await;
    let role_logged = wait_for(Duration::from_secs(5), || {
        gt_audit::read_all(&log_path)
            .map(|recs| {
                recs.iter().any(|r| {
                    r.kind == "agent.spawned"
                        && matches!(
                            r.decode::<AgentEvent>(),
                            Ok(AgentEvent::Spawned { ref session, role: SessionRole::Dog(DogKind::Dog), .. })
                                if session == "hq-dog-rex"
                        )
                })
            })
            .unwrap_or(false)
    })
    .await;

    // --- stop ---
    channel
        .emit(&serde_json::to_vec(&serde_json::json!({"name":"rex","action":"stop"})).unwrap())
        .unwrap();
    let stopped = wait_for(Duration::from_secs(5), || !probe.has_session("hq-dog-rex")).await;
    let end_logged = wait_for(Duration::from_secs(5), || {
        gt_audit::read_all(&log_path)
            .map(|recs| recs.iter().any(|r| r.kind == "agent.session_end"))
            .unwrap_or(false)
    })
    .await;

    // Teardown before asserting so the private server never leaks.
    let _ = std::process::Command::new("tmux")
        .args(["-L", &socket, "kill-server"])
        .status();
    task.abort();
    root.shutdown();

    assert!(spawned, "dog spawn did not create the hq-dog-rex tmux session");
    assert!(role_logged, "dog spawn not tracked as a role=dog agent.spawned record");
    assert!(stopped, "dog stop did not kill the session");
    assert!(end_logged, "dog stop did not emit agent.session_end");
}

fn which_tmux() -> bool {
    std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
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
    p.push(format!("gt-dog-control-{}", ulid::Ulid::new()));
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
