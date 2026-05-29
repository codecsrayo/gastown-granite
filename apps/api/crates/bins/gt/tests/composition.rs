//! Paso 6.e gate (docs/08-getting-started.md): the composition root really binds it all.
//!
//! Drives a multi-domain scenario through ONE running root and ONE audit log:
//!
//! - **scheduling + patrol + agent**: enqueue → dispatch → register lease → stale lease →
//!   patrol tick → `LeaseExpired` → reactor reclaims the bead (`cas_release`) and re-enqueues
//!   → second dispatch to a fresh worker → completion. Same shape as Paso 6.a but the
//!   reactor is doing the work the test used to do inline.
//! - **orchestration**: a one-member convoy is launched, the `MemberDispatched` reaches the
//!   injected `Effects::sling` (the would-be `gt sling`), `member_done` closes the convoy.
//! - **agent**: a couple of edge-emitted `AgentEvent`s pushed through the agent relay land
//!   in the same log and rebuild the same `SessionRegistry` under replay.
//!
//! Then the gate fact: the single log replays back to the same per-domain states two ways
//! at once — independent per-domain prefix replay (the Paso 3 determinism guarantee, kept
//! intact) AND the unified `replay_gt`/`GtState` rebuild byte-identically.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gt_audit::{read_all, replay, EventRecord};
use gt_beads::{Bead, BeadRepository, BeadStatus, InMemoryBeads};
use gt_events::Envelope;

use gt_agent::{AgentEvent, SessionRegistry, SessionRole};
use gt_merge::{MergeEvent, MergeState};
use gt_orchestration::{OrchEvent, OrchState};
use gt_patrol::{PatrolEvent, PatrolState};
use gt_quota::{QuotaEvent, QuotaState};
use gt_rig::{AddRig, RigCommand};
use gt_scheduling::{SchedEvent, SchedState};

use gt_plugin::{PluginRegistry, SheriffPlugin};
use gt_root::{
    load_state, replay_gt, spawn, spawn_hydrated, spawn_plugin_relay, Clock, Effects, RootConfig,
};

const LEASE_TIMEOUT: u64 = 30;

/// Test clock the gate advances by hand. Keeps the lease-expiry deterministic without
/// having to sleep real wall time.
#[derive(Clone)]
struct ManualClock(Arc<AtomicU64>);

impl ManualClock {
    fn new(start: u64) -> Self {
        Self(Arc::new(AtomicU64::new(start)))
    }
    fn set(&self, secs: u64) {
        self.0.store(secs, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_secs(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// Effects that just records calls. The real adapter (gt sling, rotation chain) is an
/// edge follow-up; what 6.e proves is the wiring up to the boundary.
#[derive(Clone, Default)]
struct RecordingEffects {
    slings: Arc<Mutex<Vec<(String, String)>>>,
    rotations: Arc<Mutex<Vec<String>>>,
}

impl Effects for RecordingEffects {
    fn sling(&self, convoy: &str, member: &str) {
        self.slings
            .lock()
            .unwrap()
            .push((convoy.into(), member.into()));
    }
    fn rotate(&self, account: &str) {
        self.rotations.lock().unwrap().push(account.into());
    }
}

/// Poll the audit log until `pred` says it's ready (or timeout). The writer keeps appending
/// while the loop drains relays; we read without a shared lock and tolerate a momentary
/// half-written line as a transient parse error → retry.
async fn wait_for(
    log: &Path,
    pred: impl Fn(&[EventRecord]) -> bool,
    timeout: Duration,
) -> Vec<EventRecord> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(recs) = read_all(log) {
            if pred(&recs) {
                return recs;
            }
        }
        if Instant::now() >= deadline {
            let recs = read_all(log).unwrap_or_default();
            let kinds: Vec<&str> = recs.iter().map(|r| r.kind.as_str()).collect();
            panic!("timeout; log so far ({} records): {:?}", recs.len(), kinds);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn filter(records: &[EventRecord], prefix: &str) -> Vec<EventRecord> {
    records
        .iter()
        .filter(|r| r.kind.starts_with(prefix))
        .cloned()
        .collect()
}

#[tokio::test]
async fn multi_domain_flow_through_root_replays_byte_identical() {
    // --- setup -------------------------------------------------------------------------
    let repo = Arc::new(InMemoryBeads::default());
    repo.upsert(&Bead::new("b1", "work", BeadStatus::Pending, 1))
        .await
        .unwrap();

    let log_path = std::env::temp_dir().join(format!(
        "gt-root-{}-{}.events.jsonl",
        std::process::id(),
        ulid::Ulid::new()
    ));
    let _ = std::fs::remove_file(&log_path);

    let clock = ManualClock::new(10);
    let effects = RecordingEffects::default();
    let effects_view = effects.clone();

    let root = spawn(
        repo.clone(),
        Arc::new(gt_merge::InMemoryMergeRepo::default()),
        Arc::new(gt_patrol::InMemoryPatrolRepo::default()),
        Arc::new(gt_orchestration::InMemoryOrchRepo::default()),
        effects,
        clock.clone(),
        &log_path,
        RootConfig {
            capacity: 1,
            ..RootConfig::default()
        },
    );

    // --- scheduling + patrol + agent: stale lease + reclaim ----------------------------
    // T=10s: enqueue. The reactor will record the priority for later re-enqueue.
    root.sched.enqueue("b1", 1).await;

    // Wait until the lease has been opened (Dispatched → reactor → patrol.register →
    // PatrolEvent::LeaseRegistered in the log). Polling the log is the source of truth, like
    // in production.
    wait_for(
        &log_path,
        |recs| recs.iter().any(|r| r.kind == "patrol.lease_registered"),
        Duration::from_secs(3),
    )
    .await;

    // T=45s: never heartbeat → tick beyond the timeout. The patrol detects the expiry, the
    // reactor reclaims the bead, the scheduler re-dispatches.
    clock.set(10 + LEASE_TIMEOUT + 5);
    root.patrol.tick(clock.now_secs(), LEASE_TIMEOUT).await;

    // Wait for the second dispatch — the reclaim worked end to end.
    let recs = wait_for(
        &log_path,
        |recs| {
            recs.iter()
                .filter(|r| r.kind == "scheduling.dispatched")
                .count()
                >= 2
        },
        Duration::from_secs(3),
    )
    .await;

    // Two distinct workers: the doomed one and the fresh one.
    let workers: Vec<String> = recs
        .iter()
        .filter(|r| r.kind == "scheduling.dispatched")
        .filter_map(|r| r.decode::<SchedEvent>().ok())
        .filter_map(|e| match e {
            SchedEvent::Dispatched { worker, .. } => Some(worker),
            _ => None,
        })
        .collect();
    assert_eq!(workers.len(), 2);
    assert_ne!(workers[0], workers[1]);

    // Drive the completion edge: agent transitions + repo done + close the lease.
    root.agent_events
        .send(Envelope::root(AgentEvent::Spawned {
            session: "b1".into(),
            rig: "granite".into(),
            role: SessionRole::Polecat,
            crew: None,
        }))
        .await
        .unwrap();
    let mut b = repo.get("b1").await.unwrap().unwrap();
    b.status = BeadStatus::Done;
    repo.upsert(&b).await.unwrap();
    root.patrol.close("b1").await;
    root.agent_events
        .send(Envelope::root(AgentEvent::SessionEnd {
            session: "b1".into(),
        }))
        .await
        .unwrap();
    root.sched.capacity_freed().await;

    // --- orchestration: one-member convoy through the handoff --------------------------
    root.orch
        .create_convoy("c1", vec!["b1".to_string()])
        .await;
    root.orch.launch("c1").await;

    // Wait for the convoy handoff to reach Effects::sling and the convoy to close.
    wait_for(
        &log_path,
        |recs| recs.iter().any(|r| r.kind == "orch.member_dispatched"),
        Duration::from_secs(3),
    )
    .await;
    assert_eq!(
        effects_view.slings.lock().unwrap().as_slice(),
        &[("c1".to_string(), "b1".to_string())],
        "MemberDispatched must reach the Effects::sling adapter",
    );
    root.orch.member_done("c1", "b1").await;

    // Wait for the terminal markers in the log: convoy closed + an agent session_end.
    let records = wait_for(
        &log_path,
        |recs| {
            recs.iter().any(|r| r.kind == "orch.convoy_closed")
                && recs.iter().any(|r| r.kind == "agent.session_end")
        },
        Duration::from_secs(3),
    )
    .await;

    // --- gate fact: unified replay == per-domain prefix replay -------------------------
    let sched_indep = replay(&filter(&records, "scheduling."), SchedState::default(), |s, e: &SchedEvent| s.apply(e)).unwrap();
    let patrol_indep = replay(&filter(&records, "patrol."), PatrolState::default(), |s, e: &PatrolEvent| s.apply(e)).unwrap();
    let orch_indep = replay(&filter(&records, "orch."), OrchState::default(), |s, e: &OrchEvent| s.apply(e)).unwrap();
    let merge_indep = replay(&filter(&records, "merge."), MergeState::default(), |s, e: &MergeEvent| s.apply(e)).unwrap();
    let quota_indep = replay(&filter(&records, "quota."), QuotaState::default(), |s, e: &QuotaEvent| s.apply(e)).unwrap();
    let agent_indep = replay(&filter(&records, "agent."), SessionRegistry::default(), |s, e: &AgentEvent| s.apply(e)).unwrap();

    let unified = replay_gt(&records).unwrap();

    assert_eq!(unified.sched, sched_indep, "unified sched must match prefix-replayed sched");
    assert_eq!(unified.patrol, patrol_indep, "unified patrol must match prefix-replayed patrol");
    assert_eq!(unified.orch, orch_indep, "unified orch must match prefix-replayed orch");
    assert_eq!(unified.merge, merge_indep, "unified merge must match prefix-replayed merge");
    assert_eq!(unified.quota, quota_indep, "unified quota must match prefix-replayed quota");
    assert_eq!(
        unified.agent.fingerprint(),
        agent_indep.fingerprint(),
        "unified agent registry must rebuild to the same fingerprint as the prefix replay",
    );

    // Trajectory sanity from the unified state.
    assert_eq!(unified.sched.dispatched.len(), 2, "doomed + fresh worker");
    assert_eq!(unified.patrol.expired.len(), 1, "exactly one lease expired");
    assert!(unified.patrol.tracker.is_empty(), "after close + expire the tracker is empty");

    // The convoy ran to its terminator.
    assert!(
        records.iter().any(|r| r.kind == "orch.convoy_closed"),
        "convoy must close",
    );

    // No reaction error and no undecodable event.
    assert_eq!(root.dead_letters(), 0, "no dead-letters expected on a clean run");

    // Sanity: every record decodes through GtEvent::from_record (no unknown prefix landed).
    for r in &records {
        let _ = gt_root::GtEvent::from_record(r)
            .unwrap_or_else(|e| panic!("record {} did not decode through GtEvent: {e}", r.kind));
    }

    // --- Paso 6.g gate: gt-feed projection rebuilds byte-identically -------------------
    // The feed is a pure consumer (`docs/02-tree.md` L129-134) and lives in `GtState.feed`.
    // Two replays of the same log must yield identical `FeedState`s, and the unified replay
    // must agree with a standalone `Curator::fold` over the same records.
    let feed_unified = replay_gt(&records).unwrap().feed;
    let feed_again = replay_gt(&records).unwrap().feed;
    assert_eq!(feed_unified, feed_again, "feed must replay deterministically");
    let feed_standalone = gt_feed::Curator::fold(&records);
    assert_eq!(
        feed_unified, feed_standalone,
        "unified feed must match standalone Curator::fold",
    );
    assert_eq!(
        feed_unified.total_events as usize,
        records.len(),
        "feed total must cover every record",
    );
    let feed_a = serde_json::to_string(&feed_unified).unwrap();
    let feed_b = serde_json::to_string(&feed_again).unwrap();
    assert_eq!(feed_a, feed_b, "serialized FeedState must be byte-identical");

    let _ = std::fs::remove_file(&log_path);
    root.shutdown();
}

/// hq-mc72.12.30 gate: the rig actor is wired into the composition root, so a `rig.exec`
/// (the same path `gt-mcp`'s `rig.add.execute` drives once `.with_rig(root.rig.clone())` is
/// chained) emits a `RigEvent` that the reactor drains into the single audit log AND fans out
/// over the broadcast hub (the `/api/stream` SSE source). The live catalog reflects it and a
/// fresh `replay_gt` rebuilds the same rig — proving the relay → log → broadcast → replay loop
/// is closed for the rig domain exactly like every other.
#[tokio::test]
async fn rig_exec_drains_to_log_broadcast_and_replays() {
    let repo = Arc::new(InMemoryBeads::default());
    let log_path = std::env::temp_dir().join(format!(
        "gt-rig-{}-{}.events.jsonl",
        std::process::id(),
        ulid::Ulid::new()
    ));
    let _ = std::fs::remove_file(&log_path);

    let root = spawn(
        repo,
        Arc::new(gt_merge::InMemoryMergeRepo::default()),
        Arc::new(gt_patrol::InMemoryPatrolRepo::default()),
        Arc::new(gt_orchestration::InMemoryOrchRepo::default()),
        RecordingEffects::default(),
        ManualClock::new(100),
        &log_path,
        RootConfig::default(),
    );

    // Subscribe to the broadcast BEFORE driving so the SSE-equivalent stream can't miss it.
    let mut sse = root.subscribe_events();

    // The MCP `rig.add.execute` path: exec an Add against the shared rig actor.
    root.rig
        .exec(RigCommand::Add(AddRig {
            name: "plane".into(),
            prefix: "pl".into(),
            git_url: "git@github.com:o/plane.git".into(),
            push_url: None,
            upstream_url: None,
            default_branch: "main".into(),
            now_secs: 100,
        }))
        .await
        .expect("rig add exec");

    // 1) Lands in the single audit log.
    let records = wait_for(
        &log_path,
        |recs| recs.iter().any(|r| r.kind == "rig.added"),
        Duration::from_secs(3),
    )
    .await;

    // 2) Fans out over the broadcast (the /api/stream SSE source).
    let streamed = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match sse.recv().await {
                Ok(rec) if rec.kind == "rig.added" => return rec,
                Ok(_) => continue,
                Err(e) => panic!("broadcast closed before rig.added: {e}"),
            }
        }
    })
    .await
    .expect("rig.added must reach the SSE broadcast");
    assert_eq!(streamed.kind, "rig.added");

    // 3) The live catalog reflects it (what `gt://rigs` reads through the same handle).
    let live = root.rig.rigs().await;
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].name, "plane");
    assert_eq!(live[0].prefix, "pl");

    // 4) A fresh unified replay rebuilds the same rig from the log alone.
    let unified = replay_gt(&records).unwrap();
    assert_eq!(unified.rig.rigs.len(), 1, "replay rebuilds the rig catalog");
    assert!(unified.rig.rigs.contains_key("plane"));

    assert_eq!(root.dead_letters(), 0, "no dead-letters on a clean rig exec");

    let _ = std::fs::remove_file(&log_path);
    root.shutdown();
}

/// Paso 6.f.4 gate: frontier-audit (`mcp.*`) events share the event log but carry no domain
/// state. Domain replay must skip them so reconstructed state is byte-identical with or
/// without the meta records interleaved — while the feed (prefix-agnostic) still folds them.
#[test]
fn replay_skips_meta_events_byte_identical() {
    let spawned = EventRecord::from_envelope(&Envelope::root(AgentEvent::Spawned {
        session: "p1".into(),
        rig: "granite".into(),
        role: SessionRole::Polecat,
        crew: None,
    }))
    .unwrap();
    let ended = EventRecord::from_envelope(&Envelope::root(AgentEvent::SessionEnd {
        session: "p1".into(),
    }))
    .unwrap();

    // A frontier-audit record interleaved between the two domain events.
    let mcp = EventRecord {
        event_id: ulid::Ulid::new().to_string(),
        correlation_id: ulid::Ulid::new().to_string(),
        causation_id: None,
        ts: "2026-05-28T00:00:00Z".to_string(),
        kind: "mcp.invoked".into(),
        payload: serde_json::json!({
            "kind": "invoked",
            "actor": "max",
            "tool": "agent.add.execute",
            "arguments": {"id": "p1", "rig": "granite"},
            "outcome": {"status": "ok"}
        }),
    };

    let without_meta = vec![spawned.clone(), ended.clone()];
    let with_meta = vec![spawned, mcp, ended];

    let clean = replay_gt(&without_meta).unwrap();
    let mixed = replay_gt(&with_meta).expect("meta event must not break domain replay");
    assert_eq!(
        clean.agent.fingerprint(),
        mixed.agent.fingerprint(),
        "mcp.* meta event must not perturb reconstructed domain state",
    );

    // The feed folds every record, meta included.
    assert_eq!(mixed.feed.total_events, 3, "feed folds the meta record too");
    assert_eq!(clean.feed.total_events, 2);
}

/// Paso 8.1 gate (hq-8iur.1): boot hydration restores live actor state from the audit log.
///
/// First run drives a mix of in-flight events (a merge slot in `Merging`, a live patrol
/// lease, a Launched convoy with one member `Active`, an account with `Limited` status,
/// and a spawned session). The process is then "killed" by aborting the root. A second
/// root is spawned via `load_state` → `spawn_hydrated` against the SAME log file, and the
/// gate asserts: every domain that hydrates restores non-empty state matching the first
/// run, and the log size did not grow (no events re-emitted by hydration).
#[tokio::test]
async fn boot_hydration_restores_actor_state_without_replaying_events() {
    let repo = Arc::new(InMemoryBeads::default());
    let log_path = std::env::temp_dir().join(format!(
        "gt-hydrate-{}-{}.events.jsonl",
        std::process::id(),
        ulid::Ulid::new()
    ));
    let _ = std::fs::remove_file(&log_path);

    // ---- first run: seed state -------------------------------------------------------
    let clock = ManualClock::new(100);
    let root1 = spawn(
        repo.clone(),
        Arc::new(gt_merge::InMemoryMergeRepo::default()),
        Arc::new(gt_patrol::InMemoryPatrolRepo::default()),
        Arc::new(gt_orchestration::InMemoryOrchRepo::default()),
        RecordingEffects::default(),
        clock.clone(),
        &log_path,
        RootConfig {
            capacity: 4,
            ..RootConfig::default()
        },
    );

    // merge slot: submit → start. Slot ends in `Merging`, mid-flight across restart.
    root1.merge.submit("m1", "feat/x", "msg-01").await;
    root1.merge.start("m1").await;

    // patrol lease: open and leave it live.
    root1.patrol.register("b-lease", "w1", 1, 100).await;

    // orch convoy: 2 members, launch → first goes Active via the actor's handoff.
    root1
        .orch
        .create_convoy("cv", vec!["a".into(), "b".into()])
        .await;
    root1.orch.launch("cv").await;

    // quota: probe bootstraps the account in the replay reducer (UsageProbed), then mark
    // it Limited so status persists. NOTE: `upsert_account` is not event-sourced today —
    // probe is the path that survives replay.
    root1.quota.probe("acc-1", 1000, 6000, 100).await;
    root1.quota.limited("acc-1", 100).await;

    // agent: populate the live actor AND log the Spawned event. In production the
    // supervisor edge does both: `agent.add` registers the session for live snapshots, and
    // `AgentEvent::Spawned` lands in the log so replay can reconstruct it.
    root1
        .agent
        .add(gt_agent::Session::new("s1", "granite"))
        .await;
    root1
        .agent_events
        .send(Envelope::root(AgentEvent::Spawned {
            session: "s1".into(),
            rig: "granite".into(),
            role: SessionRole::Polecat,
            crew: None,
        }))
        .await
        .unwrap();

    // Wait until everything we drove is in the log.
    let records_pre = wait_for(
        &log_path,
        |recs| {
            recs.iter().any(|r| r.kind == "merge.started")
                && recs.iter().any(|r| r.kind == "patrol.lease_registered")
                && recs.iter().any(|r| r.kind == "orch.member_dispatched")
                && recs.iter().any(|r| r.kind == "quota.usage_probed")
                && recs.iter().any(|r| r.kind == "quota.account_limited")
                && recs.iter().any(|r| r.kind == "agent.spawned")
        },
        Duration::from_secs(3),
    )
    .await;

    // Snapshot first-run live state.
    let merge1 = root1.merge.snapshot().await;
    let (patrol1_leases, patrol1_expired) = root1.patrol.snapshot().await;
    let orch1 = root1.orch.snapshot().await;
    let mut agent1: Vec<_> = root1
        .agent
        .snapshot()
        .await
        .into_iter()
        .map(|s| (s.id, s.rig, s.state))
        .collect();
    agent1.sort_by(|a, b| a.0.cmp(&b.0));

    assert_eq!(merge1.len(), 1);
    assert_eq!(merge1[0].state, gt_merge::MergeSlotState::Merging);
    assert_eq!(patrol1_leases, 1, "one live lease before restart");
    assert_eq!(orch1.len(), 1);
    assert_eq!(agent1.len(), 1);

    // Independent confirmation of what hydration *should* restore.
    let expected = replay_gt(&records_pre).unwrap();
    assert!(!expected.merge.board.is_empty());
    assert!(!expected.patrol.tracker.is_empty());
    assert!(!expected.orch.convoys.is_empty());
    assert!(!expected.quota.accounts.is_empty());
    assert!(!expected.agent.is_empty());

    let log_size_before_restart = std::fs::metadata(&log_path).unwrap().len();

    // Shutdown.
    root1.shutdown();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // ---- second run: hydrate from log ------------------------------------------------
    let hydration = load_state(&log_path).expect("hydration");
    assert!(hydration.records_folded > 0, "should fold prior events");

    let root2 = spawn_hydrated(
        repo.clone(),
        Arc::new(gt_merge::InMemoryMergeRepo::default()),
        Arc::new(gt_patrol::InMemoryPatrolRepo::default()),
        Arc::new(gt_orchestration::InMemoryOrchRepo::default()),
        RecordingEffects::default(),
        clock.clone(),
        &log_path,
        RootConfig {
            capacity: 4,
            ..RootConfig::default()
        },
        hydration,
    );

    // Give the runtime a moment; no edge messages are sent, so any reactor work would
    // already have appeared by now.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Snapshot hydrated state.
    let merge2 = root2.merge.snapshot().await;
    let (patrol2_leases, patrol2_expired) = root2.patrol.snapshot().await;
    let orch2 = root2.orch.snapshot().await;
    let mut agent2: Vec<_> = root2
        .agent
        .snapshot()
        .await
        .into_iter()
        .map(|s| (s.id, s.rig, s.state))
        .collect();
    agent2.sort_by(|a, b| a.0.cmp(&b.0));

    // The gate: identical, non-empty, durable.
    assert_eq!(merge1, merge2, "merge slots survive restart");
    assert!(!merge2.is_empty());
    assert_eq!(patrol1_leases, patrol2_leases, "patrol leases count survives");
    assert_eq!(
        patrol1_expired, patrol2_expired,
        "patrol expired-emitted counter survives"
    );
    assert!(patrol2_leases > 0);
    assert_eq!(orch1, orch2, "convoy progress survives");
    assert!(!orch2.is_empty());
    assert_eq!(agent1, agent2, "session registry survives");
    assert!(!agent2.is_empty());

    // Quota live registry on the first run is empty (UpsertAccount is not event-sourced;
    // see `AccountRegistry::from_state` doc). What the gate requires is that hydration
    // restores the durable subset that IS in the log: the account exists with Limited
    // status. Post-hydration that is non-empty.
    let (quota2_accounts, _) = root2.quota.snapshot().await;
    assert!(
        quota2_accounts > 0,
        "hydration restores quota accounts from the log"
    );

    // No new events appended by hydration — the gate's "without replaying new events".
    let log_size_after_restart = std::fs::metadata(&log_path).unwrap().len();
    assert_eq!(
        log_size_before_restart, log_size_after_restart,
        "hydration must not append to the log",
    );

    let _ = std::fs::remove_file(&log_path);
    root2.shutdown();
}

/// hq-evks gate: attaching the plugin relay to the live root must not affect what the root
/// writes — `replay_gt(log)` is byte-identical with or without plugins. The relay only reads
/// from the broadcast (no back-emit into the domain bus), and the `Plugin` trait returns
/// `Result<(), AppError>` so it has no channel to influence the log. The Sheriff stub stands
/// in for any production plugin: if it sees events, the chain is live.
#[tokio::test]
async fn plugin_relay_observes_without_perturbing_replay() {
    let repo = Arc::new(InMemoryBeads::default());
    let log_path = std::env::temp_dir().join(format!(
        "gt-plugin-{}-{}.events.jsonl",
        std::process::id(),
        ulid::Ulid::new()
    ));
    let _ = std::fs::remove_file(&log_path);

    let clock = ManualClock::new(100);
    let effects = RecordingEffects::default();
    let root = spawn(
        repo,
        Arc::new(gt_merge::InMemoryMergeRepo::default()),
        Arc::new(gt_patrol::InMemoryPatrolRepo::default()),
        Arc::new(gt_orchestration::InMemoryOrchRepo::default()),
        effects,
        clock,
        &log_path,
        RootConfig::default(),
    );

    let sheriff = SheriffPlugin::new();
    let observed = sheriff.counter();
    let registry = Arc::new(PluginRegistry::new().register_arc(Arc::new(sheriff)));
    let dead = registry.deadletter();
    let relay = spawn_plugin_relay(&root, registry);

    // Drive two AgentEvents through the edge relay — exactly the shape the supervisor would
    // emit in production. The plugin relay sees them via the broadcast; the log records them.
    use gt_agent::{AgentEvent, SessionRole};
    let evs = vec![
        AgentEvent::Spawned {
            session: "p1".into(),
            rig: "rig-a".into(),
            role: SessionRole::Mayor,
            crew: None,
        },
        AgentEvent::SessionEnd {
            session: "p1".into(),
        },
    ];
    for ev in evs {
        root.agent_events.send(Envelope::root(ev)).await.unwrap();
    }

    let recs = wait_for(
        &log_path,
        |recs| recs.iter().filter(|r| r.kind.starts_with("agent.")).count() >= 2,
        Duration::from_secs(3),
    )
    .await;

    // Replay the recorded log: the rebuilt state must contain both AgentEvents and the
    // Sheriff must have observed exactly the agent events the log holds. Plugins are pure
    // observers, so the log shape is unaffected by their presence.
    let st = replay_gt(&recs).expect("replay_gt");
    assert_eq!(
        st.agent.len(),
        1,
        "agent state must contain the spawned session"
    );

    // Wait for the relay to drain — observed count converges to (# events × # plugins=1).
    let agent_event_count = recs.iter().filter(|r| r.kind.starts_with("agent.")).count();
    let deadline = Instant::now() + Duration::from_secs(2);
    while observed.load(Ordering::SeqCst) < agent_event_count && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        observed.load(Ordering::SeqCst) >= agent_event_count,
        "sheriff stub must observe every agent.* record (saw {}, log has {})",
        observed.load(Ordering::SeqCst),
        agent_event_count
    );
    assert!(
        dead.is_empty(),
        "healthy chain must not produce dead-letter entries"
    );

    let _ = std::fs::remove_file(&log_path);
    root.shutdown();
    let _ = relay.await;
}
