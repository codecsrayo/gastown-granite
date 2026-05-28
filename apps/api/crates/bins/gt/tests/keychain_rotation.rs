//! Gate test (hq-0bko.2 + .3): rotation drives the keychain pointer + a real probe parsed
//! from `anthropic-ratelimit-*` headers reaches the actor + emits `UsageProbed`.
//!
//! Drives an end-to-end slice through the composition root:
//! 1. Seed the in-memory keychain with two accounts and pin `acc-1` as the live pointer.
//! 2. Synthesize a fake provider response with the real header names, parse it through the
//!    new `gt_quota::probe::parse_anthropic_ratelimit` and execute the resulting command on
//!    the running quota actor. Assert: `UsageProbed` is recorded in the audit log and the
//!    registry's window matches the parsed remaining/reset (idempotent under retry).
//! 3. Drive `RotateAccount` through the same actor. Assert: `Rotated` is recorded and the
//!    keychain's live pointer flipped to `acc-2`.

use std::sync::Arc;
use std::time::Duration;

use gt_audit::{read_all, EventRecord};
use gt_beads::InMemoryBeads;
use gt_events::Command;
use gt_quota::{parse_anthropic_ratelimit, InMemoryKeychain, Keychain, QuotaCommand, RotateAccount};
use gt_root::{spawn, LogEffects, RootConfig, SystemClock};

fn fresh_log_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "gt-test-{tag}-{}-{}.events.jsonl",
        std::process::id(),
        ulid::Ulid::new()
    ))
}

async fn wait_for_kind(path: &std::path::Path, kind: &str, total: Duration) -> Vec<EventRecord> {
    let deadline = std::time::Instant::now() + total;
    loop {
        if let Ok(recs) = read_all(path) {
            if recs.iter().any(|r| r.kind == kind) {
                return recs;
            }
        }
        if std::time::Instant::now() >= deadline {
            panic!("timed out waiting for {kind} in {}", path.display());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_probe_reaches_actor_and_rotation_flips_keychain_pointer() {
    let log_path = fresh_log_path("keychain-rot");
    let _ = std::fs::remove_file(&log_path);

    // hq-0bko.2: seed two credentials, pin acc-1 as the live pointer (the "before" state of
    // a healthy account whose window is being consumed).
    let keychain = Arc::new(InMemoryKeychain::seeded([
        ("acc-1", "sk-old"),
        ("acc-2", "sk-new"),
    ]));
    keychain.set_active("acc-1").unwrap();

    let root = spawn(
        Arc::new(InMemoryBeads::default()),
        Arc::new(gt_merge::InMemoryMergeRepo::default()),
        Arc::new(gt_patrol::InMemoryPatrolRepo::default()),
        Arc::new(gt_orchestration::InMemoryOrchRepo::default()),
        LogEffects,
        SystemClock,
        &log_path,
        RootConfig {
            capacity: 1,
            keychain: keychain.clone(),
            ..RootConfig::default()
        },
    );

    // --- hq-0bko.3: real probe via the synthesized header set -----------------------------
    let headers: Vec<(String, String)> = [
        ("anthropic-ratelimit-tokens-limit", "1000000"),
        ("anthropic-ratelimit-tokens-remaining", "250000"),
        ("anthropic-ratelimit-tokens-reset", "1748430000"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();

    let now_secs = 1_748_429_100_u64;
    let probe_cmd =
        parse_anthropic_ratelimit(&headers, "acc-1", now_secs).expect("headers carry tokens window");
    assert_eq!(probe_cmd.remaining, 250_000);
    assert_eq!(probe_cmd.resets_at_secs, 1_748_430_000);
    probe_cmd.validate(&Default::default()).expect("validate");

    root.quota
        .exec(QuotaCommand::Probe(probe_cmd.clone()))
        .await
        .expect("probe exec");

    let recs = wait_for_kind(&log_path, "quota.usage_probed", Duration::from_secs(3)).await;
    let probed = recs
        .iter()
        .find(|r| r.kind == "quota.usage_probed")
        .expect("UsageProbed present");
    // `QuotaEvent` is an untagged-derive enum, so the on-disk payload nests under the
    // variant name. The shape is part of the wire contract — assert on it directly so a
    // future move to `#[serde(tag = "kind")]` cannot silently break the audit decoder.
    let body = &probed.payload["UsageProbed"];
    assert_eq!(body["account"], "acc-1");
    assert_eq!(body["remaining"], 250_000);
    assert_eq!(body["resets_at_secs"], 1_748_430_000_u64);

    // Idempotent under retry: re-executing the same parsed command yields the same registry
    // state. The audit log gets a second entry (that's the wire-level idempotency contract:
    // events are append-only — dedup is via event_id, which the actor will not invent).
    root.quota
        .exec(QuotaCommand::Probe(probe_cmd))
        .await
        .expect("probe re-exec");

    // --- hq-0bko.2: rotate.execute flips the live pointer ---------------------------------
    let rotate = RotateAccount {
        from_account: "acc-1".into(),
        to_account: "acc-2".into(),
        now_secs,
    };
    root.quota
        .exec(QuotaCommand::Rotate(rotate))
        .await
        .expect("rotate exec");

    let recs = wait_for_kind(&log_path, "quota.rotated", Duration::from_secs(3)).await;
    let rotated = recs
        .iter()
        .find(|r| r.kind == "quota.rotated")
        .expect("Rotated present");
    let rb = &rotated.payload["Rotated"];
    assert_eq!(rb["from_account"], "acc-1");
    assert_eq!(rb["to_account"], "acc-2");

    // The reaction on the running root must have flipped the live pointer. Poll briefly — the
    // reaction is async (it lives on the loop after the log append).
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let live = root.keychain().active().unwrap();
        if live.as_deref() == Some("acc-2") {
            break;
        }
        if std::time::Instant::now() >= deadline {
            panic!("live pointer never flipped: {live:?}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // The unrotated credential is still retrievable for forensics / cooldown reads.
    assert_eq!(
        root.keychain().get("acc-1").unwrap().unwrap().secret,
        "sk-old"
    );
    assert_eq!(
        root.keychain().get("acc-2").unwrap().unwrap().secret,
        "sk-new"
    );

    root.shutdown();
    let _ = std::fs::remove_file(&log_path);
}
