//! Gate (hq-63az / Paso 9.E): spawn a *real* polecat process (not the `sleep` stub),
//! observe its heartbeat, `kill -9` it, restart it, and prove the whole thing is tracked as
//! `AgentEvent`s folded into the session registry.

use std::time::Duration;

use tokio::sync::mpsc;

use gt_agent::{AgentEvent, SessionRole, SessionRegistry, SessionState};
use gt_events::Envelope;
use gt_polecat::supervisor::{supervise_polecat, watch, RespawnPolicy, WatchOutcome};
use gt_polecat::{spawn_process, RestartConfig, RestartTracker, SpawnSpec};

fn tmp(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("gt-polecat-{}-{}", std::process::id(), tag))
}

/// A real, heartbeating polecat stand-in: rewrites the heartbeat file in a loop and exits
/// cleanly on SIGTERM. This is a genuine long-lived process — the point of the gate is that it
/// is NOT the old `sleep <secs>` placeholder.
fn heartbeating_spec(session: &str, hb: &std::path::PathBuf, crew: Option<&str>) -> SpawnSpec {
    SpawnSpec {
        session: session.to_string(),
        rig: "granite".to_string(),
        polecat: session.to_string(),
        crew: crew.map(str::to_string),
        workdir: std::env::temp_dir(),
        command: "sh".to_string(),
        args: vec![
            "-c".to_string(),
            "trap 'exit 0' TERM; while true; do : > \"$HB\"; sleep 0.05; done".to_string(),
        ],
        env: vec![("HB".to_string(), hb.to_string_lossy().into_owned())],
        hook_bead: None,
        issue: None,
        heartbeat: hb.clone(),
    }
}

#[tokio::test]
async fn spawn_real_heartbeat_kill9_restart_is_tracked() {
    let hb = tmp("gate-hb");
    let spec = heartbeating_spec("polecat-gate", &hb, Some("furiosa"));

    // Track session state by folding the emitted events, exactly like the live projection.
    let mut reg = SessionRegistry::default();

    // --- spawn #1: a real process, heartbeat written before we get the handle ---
    let mut p1 = spawn_process(&spec).await.unwrap();
    let pid1 = p1.pid().expect("child has a pid");
    reg.apply(&spec.spawned_event());
    let s = reg.get("polecat-gate").unwrap();
    assert_eq!(s.role, SessionRole::Polecat);
    assert_eq!(s.crew.as_deref(), Some("furiosa"), "crew attribution carried");
    assert_eq!(s.state, SessionState::Spawned);

    // --- heartbeat actually freshens (the process is alive and writing) ---
    let m0 = std::fs::metadata(&hb).unwrap().modified().unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let m1 = std::fs::metadata(&hb).unwrap().modified().unwrap();
    assert!(m1 >= m0, "live polecat keeps touching its heartbeat");

    // --- kill -9 from the outside (the real SIGKILL path, not a clean shutdown) ---
    let status = std::process::Command::new("kill")
        .args(["-9", &pid1.to_string()])
        .status()
        .unwrap();
    assert!(status.success(), "kill -9 issued");

    // The supervisor's watch detects the death (process gone) and reports Exited.
    let outcome = watch(&mut p1, Duration::from_secs(30), Duration::from_millis(20)).await;
    assert_eq!(outcome, WatchOutcome::Exited);
    reg.apply(&AgentEvent::SessionEnd {
        session: "polecat-gate".to_string(),
    });
    assert!(
        reg.get("polecat-gate").unwrap().is_terminal(),
        "killed polecat is terminal in the registry"
    );

    // --- restart: a fresh process with a new pid, re-tracked as Spawned ---
    let mut p2 = spawn_process(&spec).await.unwrap();
    let pid2 = p2.pid().expect("restarted child has a pid");
    assert_ne!(pid1, pid2, "restart is a genuinely new process");
    reg.apply(&spec.spawned_event());
    assert_eq!(reg.get("polecat-gate").unwrap().state, SessionState::Spawned);

    p2.kill().await;
    let _ = p2.wait().await;
    let _ = tokio::fs::remove_file(&hb).await;
}

#[tokio::test]
async fn supervise_loop_restarts_crashing_polecat_and_emits_events() {
    let hb = tmp("loop-hb");
    // A polecat that crashes immediately (no heartbeat): drives the restart loop fast.
    let make_spec = {
        let hb = hb.clone();
        move || SpawnSpec {
            session: "polecat-loop".to_string(),
            rig: "granite".to_string(),
            polecat: "polecat-loop".to_string(),
            crew: None,
            workdir: std::env::temp_dir(),
            command: "sh".to_string(),
            args: vec!["-c".to_string(), "exit 1".to_string()],
            env: vec![],
            hook_bead: None,
            issue: None,
            heartbeat: hb.clone(),
        }
    };

    let (tx, mut rx) = mpsc::channel::<Envelope<AgentEvent>>(16);
    // Small backoff so the single restart in this test is quick; crash-loop high enough not
    // to trip before max_restarts bounds the loop.
    let mut tracker = RestartTracker::new(RestartConfig {
        initial_backoff_secs: 1,
        crash_loop_count: 100,
        ..RestartConfig::default()
    });
    let policy = RespawnPolicy {
        stale_after: Duration::from_secs(30),
        poll: Duration::from_millis(20),
        max_restarts: 1,
    };

    supervise_polecat("granite/polecat-loop", make_spec, &mut tracker, policy, tx, || 0)
        .await
        .unwrap();

    // Fold what the supervisor emitted: two spawn attempts, two deaths.
    let mut reg = SessionRegistry::default();
    let mut spawned = 0;
    let mut ended = 0;
    while let Some(env) = rx.recv().await {
        match &env.payload {
            AgentEvent::Spawned { .. } => spawned += 1,
            AgentEvent::SessionEnd { .. } | AgentEvent::Killed { .. } => ended += 1,
            _ => {}
        }
        reg.apply(&env.payload);
    }
    assert_eq!(spawned, 2, "initial spawn + one restart");
    assert_eq!(ended, 2, "each spawn died and was recorded");
    assert!(reg.get("polecat-loop").unwrap().is_terminal());
}
