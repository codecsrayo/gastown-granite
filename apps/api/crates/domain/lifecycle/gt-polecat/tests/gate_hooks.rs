//! Gate (hq-63az / Paso 9.E hooks): spawn a polecat with a hooked bead and prove the bead is
//! visible via `tmux show-environment GT_HOOK_BEAD`. Mirrors the FIXED gg-0nb behavior: the
//! sling-attached bead must reach the polecat env and never be dropped.

use gt_polecat::tmux::{FakeTmux, Tmux, TmuxCli};
use gt_polecat::{resolve_env_hook_bead, spawn_tmux, SpawnSpec, GT_HOOK_BEAD};

fn hooked_spec(session: &str, hook_bead: Option<&str>, issue: Option<&str>) -> SpawnSpec {
    SpawnSpec {
        session: session.to_string(),
        rig: "granite".to_string(),
        polecat: session.to_string(),
        crew: Some("furiosa".to_string()),
        workdir: std::env::temp_dir(),
        command: "sleep".to_string(),
        args: vec!["30".to_string()],
        env: vec![("GT_ROLE".to_string(), "polecat".to_string())],
        hook_bead: hook_bead.map(str::to_string),
        issue: issue.map(str::to_string),
        heartbeat: std::env::temp_dir().join(format!("hb-{session}")),
    }
}

#[test]
fn fake_tmux_pins_hook_bead_into_session_env() {
    let tmux = FakeTmux::new();
    let spec = hooked_spec("gt-furiosa", Some("hq-63az"), None);
    spawn_tmux(&tmux, &spec).unwrap();

    // Direct equivalent of `tmux show-environment -t gt-furiosa GT_HOOK_BEAD`.
    assert_eq!(
        tmux.show_environment("gt-furiosa", GT_HOOK_BEAD)
            .unwrap()
            .as_deref(),
        Some("hq-63az")
    );
    // And the resolver (the polecat-side prime read) recovers it.
    let resolved = resolve_env_hook_bead(|k| tmux.show_environment("gt-furiosa", k).unwrap());
    assert_eq!(resolved.as_deref(), Some("hq-63az"));
}

#[test]
fn issue_is_used_when_no_explicit_hook_bead() {
    // The dropped-issue bug (gg-0nb): with only --issue set, the pin must still appear.
    let tmux = FakeTmux::new();
    let spec = hooked_spec("gt-nux", None, Some("hq-9"));
    spawn_tmux(&tmux, &spec).unwrap();
    assert_eq!(
        tmux.show_environment("gt-nux", GT_HOOK_BEAD)
            .unwrap()
            .as_deref(),
        Some("hq-9")
    );
}

#[test]
fn no_bead_means_no_pin() {
    let tmux = FakeTmux::new();
    let spec = hooked_spec("gt-slit", None, None);
    spawn_tmux(&tmux, &spec).unwrap();
    assert!(tmux
        .show_environment("gt-slit", GT_HOOK_BEAD)
        .unwrap()
        .is_none());
}

/// Real-tmux gate. Runs against a private `-L` socket so it never touches live polecat
/// sessions, and skips cleanly if `tmux` is not installed.
#[test]
fn real_tmux_show_environment_reports_hook_bead() {
    if which_tmux().is_none() {
        eprintln!("skipping: tmux not on PATH");
        return;
    }
    let socket = format!("gt-polecat-test-{}", std::process::id());
    let tmux = TmuxCli::new().with_socket(&socket);
    let session = format!("gt-polecat-gate-{}", std::process::id());
    let spec = hooked_spec(&session, Some("hq-63az"), None);

    spawn_tmux(&tmux, &spec).expect("spawn tmux session");
    let got = tmux.show_environment(&session, GT_HOOK_BEAD);
    // Best-effort teardown before asserting so a failure never leaks the test server.
    let _ = tmux.kill_session(&session);
    let _ = std::process::Command::new("tmux")
        .args(["-L", &socket, "kill-server"])
        .status();

    assert_eq!(
        got.unwrap().as_deref(),
        Some("hq-63az"),
        "tmux show-environment GT_HOOK_BEAD must report the slung bead"
    );
}

fn which_tmux() -> Option<()> {
    let ok = std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    ok.then_some(())
}
