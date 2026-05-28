//! hq-7pdl.1 gate: the production `Effects` adapter actually launches `gt sling` and drives
//! the rotation chain through the quota actor. Backs the deterministic fake in
//! `composition.rs` with a bin-level smoke check that the real edge wires up: a stub script
//! plays the part of the `gt` binary, and we observe the marker file it writes when invoked.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gt_beads::InMemoryBeads;
use gt_quota::{Account, AccountQuotaStatus, QuotaState};
use gt_root::{spawn, RealEffects, RootConfig, SystemClock};

#[tokio::test]
async fn sling_spawns_subprocess_with_convoy_and_member() {
    let dir = tempdir();
    let marker = dir.join("sling.marker");
    let script = dir.join("gt-stub.sh");

    // The stub records its args + the convoy/member it was called with. The real adapter
    // invokes it as `<bin> sling <convoy> <member>` (positional, as wired in effects_real.rs).
    write_script(
        &script,
        &format!(
            "#!/usr/bin/env sh\nprintf '%s\\n' \"$1 $2 $3\" > {}\n",
            quote(marker.to_str().unwrap())
        ),
    );

    let repo = Arc::new(InMemoryBeads::default());
    let log = dir.join("events.jsonl");

    let (effects, quota_slot) = RealEffects::new(script.clone());
    let root = spawn(repo, effects, SystemClock, &log, RootConfig::default());
    let _ = quota_slot.set(root.quota.clone());

    // Drive a real MemberDispatched through the orchestrator: the reactor forwards it to
    // Effects::sling, which spawns the stub.
    root.orch
        .create_convoy("c-smoke", vec!["m-1".to_string()])
        .await;
    root.orch.launch("c-smoke").await;

    let contents = wait_for_file(&marker, Duration::from_secs(5)).await;
    assert_eq!(
        contents.trim(),
        "sling c-smoke m-1",
        "stub did not see the expected sling invocation",
    );

    root.shutdown();
}

#[tokio::test]
async fn rotate_invokes_quota_command_chain_with_healthy_target() {
    let dir = tempdir();
    let script = dir.join("gt-stub.sh");
    write_script(&script, "#!/usr/bin/env sh\nexit 0\n");

    let repo = Arc::new(InMemoryBeads::default());
    let log = dir.join("events.jsonl");

    let (effects, quota_slot) = RealEffects::new(script);
    let root = spawn(repo, effects, SystemClock, &log, RootConfig::default());
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
    let script = dir.join("gt-stub.sh");
    write_script(&script, "#!/usr/bin/env sh\nexit 0\n");

    let repo = Arc::new(InMemoryBeads::default());
    let log = dir.join("events.jsonl");

    let (effects, quota_slot) = RealEffects::new(script);
    let root = spawn(repo, effects, SystemClock, &log, RootConfig::default());
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

fn tempdir() -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("gt-real-effects-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn write_script(path: &std::path::Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, body).unwrap();
    let mut perm = std::fs::metadata(path).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(path, perm).unwrap();
}

fn quote(s: &str) -> String {
    // Single-quote the path so spaces survive the shell.
    format!("'{}'", s.replace('\'', "'\\''"))
}

async fn wait_for_file(path: &std::path::Path, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(s) = std::fs::read_to_string(path) {
            if !s.is_empty() {
                return s;
            }
        }
        if Instant::now() >= deadline {
            panic!("timeout waiting for {}", path.display());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
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
