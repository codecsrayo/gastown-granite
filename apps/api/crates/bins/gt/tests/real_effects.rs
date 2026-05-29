//! hq-7pdl.1 / hq-mc72.12 D1 gate: the production `Effects` adapter spawns a real
//! Rust-managed polecat (a detached `tmux` session via `gt_polecat::PolecatLifecycle`, NOT a
//! `gt sling` subprocess) and drives the rotation chain through the quota actor. The sling
//! tests run against a private `-L` tmux socket so they never touch live sessions, and skip
//! cleanly when `tmux` is not installed. The rotate tests don't spawn, so they use a
//! `FakeTmux`-backed lifecycle.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gt_beads::InMemoryBeads;
use gt_polecat::{
    FakeTmux, PolecatLifecycle, PolecatSupervisor, RestartConfig, SpawnTemplate, TmuxCli,
    GT_HOOK_BEAD,
};
use gt_quota::{Account, AccountQuotaStatus, QuotaState};
use gt_root::{spawn, Effects, RealEffects, RootConfig, SystemClock};

/// Spawn-template whose command is a harmless long-lived process so `respawn-pane` succeeds
/// on hosts without the real coding agent installed.
fn test_template() -> SpawnTemplate {
    SpawnTemplate {
        rig: "granite".to_string(),
        prefix: "gt".to_string(),
        workdir: std::env::temp_dir(),
        command: "sleep".to_string(),
        args: vec!["30".to_string()],
        base_env: vec![("GT_ROLE".to_string(), "polecat".to_string())],
        heartbeat_dir: std::env::temp_dir(),
    }
}

fn which_tmux() -> bool {
    std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[tokio::test]
async fn sling_spawns_polecat_session_via_lifecycle() {
    if !which_tmux() {
        eprintln!("skipping: tmux not on PATH");
        return;
    }
    let dir = tempdir();
    let socket = format!("gt-real-effects-{}", ulid::Ulid::new());
    let tmux = TmuxCli::new().with_socket(&socket);
    let lifecycle = PolecatLifecycle::new(Box::new(TmuxCli::new().with_socket(&socket)), test_template());

    let repo = Arc::new(InMemoryBeads::default());
    let log = dir.join("events.jsonl");
    let (effects, quota_slot) = RealEffects::new(lifecycle, test_polecat_supervisor());
    let root = spawn(
        repo,
        Arc::new(gt_merge::InMemoryMergeRepo::default()),
        Arc::new(gt_patrol::InMemoryPatrolRepo::default()),
        Arc::new(gt_orchestration::InMemoryOrchRepo::default()),
        effects,
        SystemClock,
        &log,
        RootConfig::default(),
    );
    let _ = quota_slot.set(root.quota.clone());

    // Drive a real MemberDispatched through the orchestrator: the reactor forwards it to
    // Effects::sling, which spawns the tmux-backed polecat `gt-m-1` (prefix + sanitized member).
    root.orch
        .create_convoy("c-smoke", vec!["m-1".to_string()])
        .await;
    root.orch.launch("c-smoke").await;

    let appeared = wait_for(Duration::from_secs(5), || {
        use gt_polecat::Tmux;
        tmux.has_session("gt-m-1")
    })
    .await;

    // Capture env before teardown so a failure still cleans up the private server.
    let convoy = {
        use gt_polecat::Tmux;
        tmux.show_environment("gt-m-1", "GT_CONVOY").ok().flatten()
    };
    let _ = std::process::Command::new("tmux")
        .args(["-L", &socket, "kill-server"])
        .status();
    root.shutdown();

    assert!(appeared, "sling did not create the tmux polecat session gt-m-1");
    assert_eq!(convoy.as_deref(), Some("c-smoke"), "GT_CONVOY not carried into the session");
}

#[tokio::test]
async fn sling_pins_dispatched_bead_as_gt_hook_bead() {
    // hq-63az / gg-0nb gate: the slung polecat must carry GT_HOOK_BEAD = the member, so the
    // deferred-spawn path can hand the polecat its hook without a flippable bd read.
    if !which_tmux() {
        eprintln!("skipping: tmux not on PATH");
        return;
    }
    let dir = tempdir();
    let socket = format!("gt-real-effects-{}", ulid::Ulid::new());
    let tmux = TmuxCli::new().with_socket(&socket);
    let lifecycle = PolecatLifecycle::new(Box::new(TmuxCli::new().with_socket(&socket)), test_template());

    let repo = Arc::new(InMemoryBeads::default());
    let log = dir.join("events.jsonl");
    let (effects, quota_slot) = RealEffects::new(lifecycle, test_polecat_supervisor());
    let root = spawn(
        repo,
        Arc::new(gt_merge::InMemoryMergeRepo::default()),
        Arc::new(gt_patrol::InMemoryPatrolRepo::default()),
        Arc::new(gt_orchestration::InMemoryOrchRepo::default()),
        effects,
        SystemClock,
        &log,
        RootConfig::default(),
    );
    let _ = quota_slot.set(root.quota.clone());

    root.orch
        .create_convoy("c-hook", vec!["hq-63az".to_string()])
        .await;
    root.orch.launch("c-hook").await;

    let appeared = wait_for(Duration::from_secs(5), || {
        use gt_polecat::Tmux;
        tmux.has_session("gt-hq-63az")
    })
    .await;
    let hook = {
        use gt_polecat::Tmux;
        tmux.show_environment("gt-hq-63az", GT_HOOK_BEAD).ok().flatten()
    };
    let _ = std::process::Command::new("tmux")
        .args(["-L", &socket, "kill-server"])
        .status();
    root.shutdown();

    assert!(appeared, "sling did not create the tmux polecat session gt-hq-63az");
    assert_eq!(
        hook.as_deref(),
        Some("hq-63az"),
        "slung polecat did not carry GT_HOOK_BEAD = member",
    );
}

#[tokio::test]
async fn rotate_invokes_quota_command_chain_with_healthy_target() {
    let dir = tempdir();
    let repo = Arc::new(InMemoryBeads::default());
    let log = dir.join("events.jsonl");

    let (effects, quota_slot) = RealEffects::new(fake_lifecycle(), test_polecat_supervisor());
    let root = spawn(
        repo,
        Arc::new(gt_merge::InMemoryMergeRepo::default()),
        Arc::new(gt_patrol::InMemoryPatrolRepo::default()),
        Arc::new(gt_orchestration::InMemoryOrchRepo::default()),
        effects,
        SystemClock,
        &log,
        RootConfig::default(),
    );
    let _ = quota_slot.set(root.quota.clone());

    // Two healthy accounts: "from" is the one the predictor flagged, "to" is the rotation
    // target the adapter must pick (first healthy != from).
    root.quota.upsert_account(Account::new("acct-from")).await;
    root.quota.upsert_account(Account::new("acct-to")).await;

    // Reactive limit event: the reactor forwards to Effects::rotate(account=acct-from).
    // The adapter snapshots accounts, picks acct-to, and runs QuotaCommand::Rotate. The
    // quota actor then emits QuotaEvent::Rotated.
    root.quota.limited("acct-from", 1234).await;

    // Wait until the audit log shows the rotation event the chain produced.
    let log_path = root.log_path().to_path_buf();
    let saw_rotated = wait_for_predicate(Duration::from_secs(5), || {
        let recs = match gt_audit::read_all(&log_path) {
            Ok(r) => r,
            Err(_) => return false,
        };
        let state = recs
            .iter()
            .filter(|r| r.kind.starts_with("quota."))
            .fold(QuotaState::default(), |mut s, r| {
                if let Ok(ev) = r.decode::<gt_quota::QuotaEvent>() {
                    s.apply(&ev);
                }
                s
            });
        state
            .rotations
            .iter()
            .any(|(from, to)| from == "acct-from" && to == "acct-to")
    })
    .await;

    assert!(
        saw_rotated,
        "rotation chain never produced a Rotated event for acct-from -> acct-to",
    );

    // The source account is parked in cooldown by `RotateAccount::execute`.
    let accounts = root.quota.accounts().await;
    let from = accounts.iter().find(|a| a.id == "acct-from").unwrap();
    assert_eq!(from.status, AccountQuotaStatus::Cooldown);

    root.shutdown();
}

#[tokio::test]
async fn rotate_with_no_healthy_target_is_a_noop() {
    let dir = tempdir();
    let repo = Arc::new(InMemoryBeads::default());
    let log = dir.join("events.jsonl");

    let (effects, quota_slot) = RealEffects::new(fake_lifecycle(), test_polecat_supervisor());
    let root = spawn(
        repo,
        Arc::new(gt_merge::InMemoryMergeRepo::default()),
        Arc::new(gt_patrol::InMemoryPatrolRepo::default()),
        Arc::new(gt_orchestration::InMemoryOrchRepo::default()),
        effects,
        SystemClock,
        &log,
        RootConfig::default(),
    );
    let _ = quota_slot.set(root.quota.clone());

    // Only one account: no healthy target for rotation. The adapter must log and skip.
    root.quota.upsert_account(Account::new("solo")).await;
    root.quota.limited("solo", 4321).await;

    // Sleep briefly so the spawned task runs.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // No quota.rotated record should appear; the AccountLimited record does.
    let recs = gt_audit::read_all(root.log_path()).unwrap_or_default();
    assert!(
        recs.iter().any(|r| r.kind == "quota.account_limited"),
        "AccountLimited record missing",
    );
    assert!(
        !recs.iter().any(|r| r.kind == "quota.rotated"),
        "rotation must not fire without a target",
    );

    root.shutdown();
}

/// A lifecycle that never reaches a real tmux server — the rotate tests don't sling.
fn fake_lifecycle() -> PolecatLifecycle {
    PolecatLifecycle::new(Box::new(FakeTmux::new()), test_template())
}

#[tokio::test]
async fn sling_registers_with_supervisor_and_release_unwatches() {
    // hq-mc72.12 C5b: a slung polecat is registered with the supervisor (so a dead session is
    // re-slung), and Effects::release (driven by Merge::Merged/Failed) unwatches it.
    let supervisor = Arc::new(PolecatSupervisor::new(
        Arc::new(FakeTmux::new()),
        RestartConfig::default(),
        u32::MAX,
    ));
    let lifecycle = PolecatLifecycle::new(Box::new(FakeTmux::new()), test_template());
    let (effects, _slot) = RealEffects::new(lifecycle, supervisor.clone());

    // sling is fire-and-forget (spawn_blocking); poll until the watch lands.
    effects.sling("cv-c5", "hq-c5");
    let watched = wait_for(Duration::from_secs(5), || supervisor.watched_count() == 1).await;
    assert!(watched, "sling did not register the polecat with the supervisor");

    // Work terminated → release unwatches it (no resurrection).
    effects.release("hq-c5");
    assert_eq!(supervisor.watched_count(), 0, "release did not unwatch the polecat");
}

fn test_polecat_supervisor() -> Arc<gt_polecat::PolecatSupervisor> {
    Arc::new(gt_polecat::PolecatSupervisor::new(
        Arc::new(FakeTmux::new()),
        gt_polecat::RestartConfig::default(),
        u32::MAX,
    ))
}

fn tempdir() -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("gt-real-effects-{}", ulid::Ulid::new()));
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

async fn wait_for_predicate(timeout: Duration, mut pred: impl FnMut() -> bool) -> bool {
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
