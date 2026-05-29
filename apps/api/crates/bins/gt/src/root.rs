//! The composition root: spawn every domain actor, hand each one a relay, drain all relays
//! in a single async loop, append every event to one audit log, and fire the cross-domain
//! reactions the earlier steps left to "the composition root" (Pasos 6.a/6.b/6.c/6.d).
//!
//! Why a select-loop and not the sync `Bus`: the reactions here are I/O at the edge (they
//! `.await` actor handles and the Dolt-port repo). `gt-bus` is the **sync, in-core** fan-out
//! for pure same-type handlers; cross-domain effects that await live on the async edge — the
//! loop below — exactly as the Paso 6.a gate test already demonstrated. The loop is the only
//! writer to the log, so events land in a single total order with no interleaving.
//!
//! Side effects that reach outside the process (`gt sling` a convoy member, rotate an
//! account) go through the [`Effects`] port; the clock is the [`Clock`] port. Both are
//! injected, so `main` wires the real adapters and the gate injects deterministic fakes —
//! the ports & adapters rule (`docs/01-architecture.md`), keeping the loop replay-able.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use gt_audit::{read_all, EventRecord, EventStore, JsonlWriter};
use gt_beads::{Bead, BeadRepository, BeadStatus};
use gt_bus::DeadLetterEntry;
use gt_events::{AppError, Envelope, EventKind};
use gt_notify::{Channel, Notification, Notifier, Signal};
use gt_plugin::PluginRegistry;

use gt_agent::actor::{self as agent_actor, AgentHandle};
use gt_agent::{AgentEvent, Session, SessionState, SessionWriter};
use gt_mayor::actor::{self as mayor_actor, MayorHandle};
use gt_mayor::{InMemoryMayorRepo, MayorEvent};
use gt_merge::actor::{self as merge_actor, MergeHandle};
use gt_merge::{MergeBoard, MergeEvent, MergeRepository};
use gt_orchestration::actor::{self as orch_actor, OrchHandle};
use gt_orchestration::{ConvoyBoard, OrchEvent, OrchRepository};
use gt_patrol::actor::{self as patrol_actor, PatrolHandle};
use gt_patrol::{LeaseTracker, PatrolEvent, PatrolRepository};
use gt_sheriff::actor::{self as sheriff_actor, SheriffHandle};
use gt_sheriff::{InMemorySheriffRepo, SheriffEvent};
use gt_deacon::actor::{self as deacon_actor, DeaconHandle};
use gt_deacon::{DeaconEvent, InMemoryDeaconRepo};
use gt_refinery::actor::{self as refinery_actor, RefineryHandle};
use gt_refinery::{InMemoryRefineryRepo, RefineryEvent};
use gt_witness::actor::{self as witness_actor, WitnessHandle};
use gt_witness::{InMemoryWitnessRepo, WitnessEvent};
use gt_quota::actor::{self as quota_actor, QuotaHandle};
use gt_quota::{AccountRegistry, InMemoryKeychain, Keychain, ModelWeights, QuotaEvent};
use gt_rig::actor::{self as rig_actor, RigHandle};
use gt_rig::{RigCatalog, RigEvent};
use gt_scheduling::actor::{self as sched_actor, SchedHandle};
use gt_scheduling::SchedEvent;

use crate::event::{replay_gt, GtEvent, GtState};

/// Wall-clock at the edge. The core never reads it; the root stamps `now_secs` onto the
/// messages it sends to the actors (e.g. registering a lease), and that value then travels
/// in the recorded event — so replay stays deterministic.
pub trait Clock: Send + 'static {
    fn now_secs(&self) -> u64;
}

/// Real clock for the binary.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_secs(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}

/// Outward-facing effects the root triggers in response to domain decisions. These are the
/// genuine side effects that cross the process boundary; everything else is in-process actor
/// messaging. The real adapter runs subprocesses / the rotation chain; tests inject a fake.
pub trait Effects: Send + 'static {
    /// Convoy handoff: run `member` now (a `gt sling <member>` in production). Emitted from
    /// `OrchEvent::MemberDispatched`.
    fn sling(&self, convoy: &str, member: &str);
    /// An account will block (predicted) or just blocked (reactive): rotate off it. Emitted
    /// from `QuotaEvent::BlockPredicted` / `AccountLimited`.
    fn rotate(&self, account: &str);
    /// Work for `member` terminated (merged or failed): release any edge resources tied to it
    /// — in production, stop supervising its polecat so a completed session is not re-slung
    /// (hq-mc72.12 C5). Default no-op so log / test adapters need not implement it.
    fn release(&self, _member: &str) {}
}

/// Default effects: log to stderr. The real subprocess/rotation adapter is a follow-up
/// (a `bins/`-level edge); the wiring it plugs into is what Paso 6.e delivers.
pub struct LogEffects;

impl Effects for LogEffects {
    fn sling(&self, convoy: &str, member: &str) {
        eprintln!("[gt] handoff: sling convoy={convoy} member={member}");
    }
    fn rotate(&self, account: &str) {
        eprintln!("[gt] quota: rotate off account={account}");
    }
}

/// Default [`Notifier`] (hq-mysw): log to stderr. The escalation status bead is the durable
/// record regardless; this fallback just means no mail goes out until the bin wires the real
/// bead-backed adapter (`MailNotifier`) into [`RootConfig::notifier`].
pub struct LogNotifier;

impl Notifier for LogNotifier {
    fn notify(&self, n: &Notification) {
        eprintln!(
            "[gt] notify [{}/{}] {}",
            n.signal.tag(),
            n.severity.as_str(),
            n.subject
        );
    }
}

/// Handle to a running root. Holds clones of every actor handle plus the agent relay so the
/// edge (timers, probes, the channel watcher, the test) can drive the system; the internal
/// loop observes what the actors emit, logs it, and reacts.
pub struct RootHandle<R: BeadRepository + Clone> {
    pub sched: SchedHandle,
    pub patrol: PatrolHandle,
    pub merge: MergeHandle,
    pub orch: OrchHandle,
    pub quota: QuotaHandle,
    /// hq-mc72.12.30 — rig catalog actor (orchestrator's rig registry). gt-mcp's `rig.*`
    /// tools + `gt://rigs` resource drive this handle once `main` chains `.with_rig`.
    pub rig: RigHandle,
    pub agent: AgentHandle,
    /// hq-92z9 Paso 9.D — Sheriff watchdog. Operators register watches via this handle
    /// (`SheriffCommand::Register`/`Clear`); observations are fed automatically from
    /// the reactor's ingest path (every non-`sheriff.*` event kind).
    pub sheriff: SheriffHandle,
    /// hq-92z9 Paso 9.D — Deacon drain coordination. Producers (SIGTERM handler at the
    /// edge, root reactions that observe in-flight domain events) call
    /// `gt_deacon::deacon::{begin_drain, track, finish}` against this handle.
    pub deacon: DeaconHandle,
    /// hq-92z9 Paso 9.D — Refinery projection over merge-ready observations. Updated via
    /// `gt_refinery::refinery::{observe, mark_dispatched}` from root reactions; coexists
    /// with the existing `gt_merge::refinery` channel watcher (which stays authoritative
    /// for driving the merge slot).
    pub refinery: RefineryHandle,
    /// hq-92z9 Paso 9.D — Witness escalation observer. Operators register worker watches
    /// (`WitnessCommand::Watch`) and drive ticks (`Tick`/`Clear`) from the edge (gt-mcp
    /// / CLI). No automatic cross-domain wiring yet — the actor only acts on explicit
    /// commands sent through this handle.
    pub witness: WitnessHandle,
    /// hq-92z9 Paso 9.D — Mayor orchestration-loop role. Operators push delegations via
    /// this handle (`MayorCommand::Delegate`/`Acknowledge`/`Resolve`/`Withdraw`). The
    /// periodic auto-delegate loop is a follow-up; this handle is the substrate it will
    /// drive.
    pub mayor: MayorHandle,
    /// Relay for edge producers of agent events (the supervisor's `SessionEnd`, the spawn
    /// edge's `Spawned`). The agent actor has no relay of its own by design; its events
    /// reach the log through here.
    pub agent_events: mpsc::Sender<Envelope<AgentEvent>>,
    pub repo: R,
    log_path: PathBuf,
    dead_count: Arc<AtomicUsize>,
    /// Broadcast hub for every appended [`EventRecord`]. Read-side consumers (notably
    /// `gt-web`'s SSE) call [`RootHandle::subscribe_events`] per connection. The reactor is
    /// the single writer; readers that lag get `Lagged` and resync from the snapshot, exactly
    /// like the doc's bus -> broadcast -> SSE bridge (`docs/07-frontend.md`).
    events: broadcast::Sender<EventRecord>,
    /// Shared keychain handle so callers (the gate test, gt-mcp's rotate.execute reaction)
    /// can inspect the live pointer after a rotation lands.
    keychain: Arc<dyn Keychain>,
    join: JoinHandle<()>,
}

impl<R: BeadRepository + Clone> RootHandle<R> {
    /// The single audit log every domain event is appended to.
    pub fn log_path(&self) -> &Path {
        &self.log_path
    }

    /// Number of dead-letter entries so far (reaction failures + undecodable events). A
    /// healthy run keeps this at 0.
    pub fn dead_letters(&self) -> usize {
        self.dead_count.load(Ordering::SeqCst)
    }

    /// Subscribe to the live event stream. Each subscriber gets its own [`broadcast::Receiver`];
    /// drop it to unsubscribe. Used by `gt-web` to feed an SSE connection.
    pub fn subscribe_events(&self) -> broadcast::Receiver<EventRecord> {
        self.events.subscribe()
    }

    /// Clone of the broadcast sender — `gt-web` needs it so each request can subscribe lazily.
    pub fn events_sender(&self) -> broadcast::Sender<EventRecord> {
        self.events.clone()
    }

    /// Number of current live subscribers — useful for tests and `/health`-style endpoints.
    pub fn event_subscribers(&self) -> usize {
        self.events.receiver_count()
    }

    /// Borrow the running keychain. Lets the bin's MCP frontier read/write credentials
    /// without re-spawning an adapter, and the gate test assert on the live pointer.
    pub fn keychain(&self) -> Arc<dyn Keychain> {
        self.keychain.clone()
    }

    /// Stop the loop. The actors stop when their handles drop.
    pub fn shutdown(self) {
        self.join.abort();
    }
}

/// Configuration for the root.
pub struct RootConfig {
    /// Concurrent polecat capacity for the scheduler.
    pub capacity: usize,
    /// Per-model cost weights for the quota domain (empty = identity fallback).
    pub model_weights: HashMap<String, ModelWeights>,
    /// Ring size of the event broadcast hub. Default 1024 matches `docs/07-frontend.md`.
    pub event_buffer: usize,
    /// Platform credential store the rotation chain swaps on `Quota::Rotated`. Defaults to
    /// the in-memory adapter so the bin boots end-to-end on a host without a configured
    /// secret store; the production bin replaces it with `gt_quota::keychain::linux::LinuxKeychain`.
    pub keychain: Arc<dyn Keychain>,
    /// Operator-signal sink (hq-mysw). The reactor routes escalation / quota-block /
    /// merge-stuck signals through this port. Defaults to [`LogNotifier`]; the production bin
    /// installs the bead-backed `MailNotifier`, the gate injects `gt_notify::FakeNotifier`.
    pub notifier: Arc<dyn Notifier>,
    /// hq-mc72.12 C2 Witness — inactivity budget (seconds) applied when the reactor
    /// auto-watches a worker on `PatrolEvent::LeaseExpired`. Once the next witness tick lands
    /// past this threshold the actor raises `EscalationRaised`, which the reactor turns into
    /// a status bead + notification through the existing hq-mysw escalation pipeline. Default
    /// 300s (5 min).
    pub witness_threshold_secs: u64,
}

impl Default for RootConfig {
    fn default() -> Self {
        Self {
            capacity: 4,
            model_weights: HashMap::new(),
            event_buffer: 1024,
            keychain: Arc::new(InMemoryKeychain::new()),
            notifier: Arc::new(LogNotifier),
            witness_threshold_secs: 300,
        }
    }
}

/// Spawn the whole system: actors + the draining loop. `repo` is the Dolt-port (or its
/// in-memory stand-in), `effects`/`clock` are the injected edges, `log_path` is the single
/// audit log. `merge_repo` / `patrol_repo` / `orch_repo` are the domain-state persistence
/// ports introduced in epic hq-bdn8 — Dolt-backed when `GT_DOLT_URL` is set, in-memory
/// otherwise.
///
/// Thin wrapper over [`spawn_hydrated`] with empty initial state: bookkeeping starts fresh
/// (no boot hydration). Production binaries should call [`spawn_hydrated`] with the
/// reducer rebuilt from the audit log to survive restarts (hq-8iur.1).
pub fn spawn<R, MR, PR, OR, FX, CK>(
    repo: R,
    merge_repo: MR,
    patrol_repo: PR,
    orch_repo: OR,
    effects: FX,
    clock: CK,
    log_path: impl Into<PathBuf>,
    config: RootConfig,
) -> RootHandle<R>
where
    R: BeadRepository + Clone + 'static,
    MR: MergeRepository + 'static,
    PR: PatrolRepository + 'static,
    OR: OrchRepository + 'static,
    FX: Effects,
    CK: Clock,
{
    spawn_hydrated(
        repo,
        merge_repo,
        patrol_repo,
        orch_repo,
        effects,
        clock,
        log_path,
        config,
        HydrationState::default(),
    )
}

/// Rebuild the per-actor seed state from the audit log at `log_path` (boot hydration,
/// hq-8iur.1). Reads every record via [`gt_audit::read_all`] and folds them through
/// `replay_gt` — the same deterministic reducer that powers the Step 3 gate. Returns an
/// empty state if the log file doesn't exist (fresh install).
///
/// The result is passed to [`spawn_hydrated`] so the actors start with merge slots, patrol
/// leases, convoy progress and quota account status restored before serving — without
/// re-emitting any of the events to the log.
pub fn load_state(log_path: impl AsRef<Path>) -> Result<HydrationState, AppError> {
    let records = read_all(log_path.as_ref())?;
    let state = replay_gt(&records)?;
    Ok(HydrationState {
        records_folded: records.len(),
        state,
    })
}

/// Aggregate of every actor's seed state plus a small bit of telemetry (how many records
/// were folded to produce it). Built by [`load_state`], consumed by [`spawn_hydrated`].
#[derive(Debug, Default)]
pub struct HydrationState {
    pub records_folded: usize,
    pub state: GtState,
}

/// Sessions write-path projector (hq-8iur.2). Subscribes to the root's broadcast and mirrors
/// every `AgentEvent` lifecycle transition into the canonical sessions store via the
/// [`SessionWriter`] port — so the read-side (`SessionQueries`) owns the truth instead of the
/// Go `gt sling` writer. The reactor is the single log writer; this consumer is read-only of
/// the broadcast, never publishing back (CQRS, `docs/07-frontend.md`).
///
/// Writes are idempotent: `Spawned` re-upserts the row (role/crew included, hq-8iur.7),
/// `SessionEnd`/`Killed` update the state by id, `Heartbeat` is liveness only and touches no
/// row. A write failure is logged and skipped — the audit log remains authoritative and a
/// later event re-converges the row.
pub fn spawn_sessions_projector<R, W>(root: &RootHandle<R>, writer: Arc<W>) -> JoinHandle<()>
where
    R: BeadRepository + Clone + 'static,
    W: SessionWriter + 'static,
{
    let mut rx = root.subscribe_events();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(rec) => {
                    if !rec.kind.starts_with("agent.") {
                        continue;
                    }
                    let ev: AgentEvent = match rec.decode() {
                        Ok(e) => e,
                        Err(e) => {
                            eprintln!("[gt] sessions projector decode failed ({}): {e}", rec.kind);
                            continue;
                        }
                    };
                    let res = match ev {
                        AgentEvent::Spawned { session, rig, role, crew } => {
                            writer
                                .upsert(&Session::with_role(session, rig, role, crew))
                                .await
                        }
                        AgentEvent::SessionEnd { session } => {
                            writer.set_state(&session, SessionState::Done).await
                        }
                        AgentEvent::Killed { session, .. } => {
                            writer.set_state(&session, SessionState::Killed).await
                        }
                        // Liveness only — no row change (mirrors `SessionRegistry::apply`).
                        AgentEvent::Heartbeat { .. } => Ok(()),
                    };
                    if let Err(e) = res {
                        eprintln!("[gt] sessions projector write failed ({}): {e}", rec.kind);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    eprintln!("[gt] sessions projector lagged by {n} events (catching up)");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

/// Observer plugin relay (hq-evks). Subscribes the registered plugins to the root's
/// `EventRecord` broadcast in a dedicated tokio task; per-event fan-out runs sequentially in
/// registration order, errors land in the registry's [`gt_plugin::PluginDeadLetter`] and the
/// chain keeps going. The relay is strictly read-only — plugins observe the audit log shape,
/// they never publish back into the domain bus — which is what keeps `replay_gt` byte-
/// identical with or without plugins attached (the gate criterion in Paso 9.B). Sheriff /
/// watchdog behavior is registered against this relay; 9.D replaces the stub plugin without
/// changing the wiring.
pub fn spawn_plugin_relay<R>(
    root: &RootHandle<R>,
    registry: Arc<PluginRegistry>,
) -> JoinHandle<()>
where
    R: BeadRepository + Clone + 'static,
{
    gt_plugin::spawn_plugin_relay(root.subscribe_events(), registry)
}

/// Spawn the whole system with explicit boot hydration (hq-8iur.1). The actors are seeded
/// with the live owner types reconstructed from `hydration.state`, so an audit-log fold
/// rebuilds in-flight merge slots / patrol leases / convoy progress / quota account status
/// **before** the actor starts processing edge messages.
///
/// Scheduling is deliberately not hydrated: its in-flight count and queue depth depend on
/// cross-domain `capacity_freed` signals that are not represented in the log (the durable
/// truth for pending work is Dolt's `pending` beads, which the edge re-pumps via Enqueue).
#[allow(clippy::too_many_arguments)]
pub fn spawn_hydrated<R, MR, PR, OR, FX, CK>(
    repo: R,
    merge_repo: MR,
    patrol_repo: PR,
    orch_repo: OR,
    effects: FX,
    clock: CK,
    log_path: impl Into<PathBuf>,
    config: RootConfig,
    hydration: HydrationState,
) -> RootHandle<R>
where
    R: BeadRepository + Clone + 'static,
    MR: MergeRepository + 'static,
    PR: PatrolRepository + 'static,
    OR: OrchRepository + 'static,
    FX: Effects,
    CK: Clock,
{
    let log_path = log_path.into();

    // One relay per event-producing domain. The actor owns its state and emits here; the
    // loop below drains every relay into the single log + the reactions.
    let (sched_tx, sched_rx) = mpsc::channel::<Envelope<SchedEvent>>(256);
    let (patrol_tx, patrol_rx) = mpsc::channel::<Envelope<PatrolEvent>>(256);
    let (merge_tx, merge_rx) = mpsc::channel::<Envelope<MergeEvent>>(256);
    let (orch_tx, orch_rx) = mpsc::channel::<Envelope<OrchEvent>>(256);
    let (quota_tx, quota_rx) = mpsc::channel::<Envelope<QuotaEvent>>(256);
    let (rig_tx, rig_rx) = mpsc::channel::<Envelope<RigEvent>>(256);
    let (agent_tx, agent_rx) = mpsc::channel::<Envelope<AgentEvent>>(256);
    let (sheriff_tx, sheriff_rx) = mpsc::channel::<Envelope<SheriffEvent>>(256);
    let (deacon_tx, deacon_rx) = mpsc::channel::<Envelope<DeaconEvent>>(256);
    let (refinery_tx, refinery_rx) = mpsc::channel::<Envelope<RefineryEvent>>(256);
    let (witness_tx, witness_rx) = mpsc::channel::<Envelope<WitnessEvent>>(256);
    let (mayor_tx, mayor_rx) = mpsc::channel::<Envelope<MayorEvent>>(256);

    // Convert the replay reducer state into each actor's live owner type. The conversions
    // live inside the domain crates (Ports & Adapters: the live-vs-replay distinction is a
    // domain concern). For agent the reducer IS the owner type — move it in directly.
    let GtState {
        agent: agent_state,
        sched: _,
        patrol: patrol_state,
        merge: merge_state,
        quota: quota_state,
        orch: orch_state,
        sheriff: sheriff_initial,
        deacon: deacon_initial,
        refinery: refinery_initial,
        witness: witness_initial,
        mayor: mayor_initial,
        rig: rig_state,
        feed: _,
    } = hydration.state;
    let merge_initial = MergeBoard::from_state(&merge_state);
    let patrol_initial = LeaseTracker::from_state(&patrol_state);
    let patrol_expired_seen = patrol_state.expired.len();
    let orch_initial = ConvoyBoard::from_state(&orch_state);
    let quota_initial = AccountRegistry::from_state(&quota_state);
    let quota_predictions_seen = quota_state.predictions.len();
    let rig_initial = RigCatalog::from_state(&rig_state);

    let sched = sched_actor::spawn(repo.clone(), sched_tx, config.capacity);
    let patrol = patrol_actor::spawn_hydrated(patrol_repo, patrol_tx, patrol_initial, patrol_expired_seen);
    let merge = merge_actor::spawn_hydrated(merge_repo, merge_tx, merge_initial);
    let orch = orch_actor::spawn_hydrated(orch_repo, orch_tx, orch_initial);
    let quota = quota_actor::spawn_hydrated(quota_tx, config.model_weights, quota_initial, quota_predictions_seen);
    // Rig catalog: hydrate the live catalog from the replayed RigState reducer (hq-8iur.1
    // boot-hydration pattern), then drain its relay into the reactor like every other domain.
    let rig = rig_actor::spawn_hydrated(rig_tx, rig_initial);
    let agent = agent_actor::spawn_hydrated(256, agent_state);
    // Sheriff repo is in-memory for now — the Dolt adapter lands when a per-watch panel
    // surfaces in `gt-web`. The reducer + replay are already authoritative for state.
    let sheriff = sheriff_actor::spawn_hydrated(
        InMemorySheriffRepo::default(),
        sheriff_tx,
        sheriff_initial,
    );
    // Deacon repo is in-memory for now — the Dolt adapter lands when the operator panel
    // surfaces pending drain items.
    let deacon = deacon_actor::spawn_hydrated(
        InMemoryDeaconRepo::default(),
        deacon_tx,
        deacon_initial,
    );
    // Refinery repo is in-memory; Dolt adapter lands when an operator panel surfaces the
    // refinery-side projection alongside the merge slot view.
    let refinery = refinery_actor::spawn_hydrated(
        InMemoryRefineryRepo::default(),
        refinery_tx,
        refinery_initial,
    );
    // Witness repo is in-memory for now — same rationale as Sheriff. The reducer + replay
    // are authoritative for state; the Dolt adapter lands when a per-target panel surfaces
    // in `gt-web`.
    let witness = witness_actor::spawn_hydrated(
        InMemoryWitnessRepo::default(),
        witness_tx,
        witness_initial,
    );
    // Mayor repo is in-memory pending the orchestration-loop follow-up that wires the
    // Dolt-backed delegation store. Reducer + replay are authoritative for state today.
    let mayor = mayor_actor::spawn_hydrated(
        InMemoryMayorRepo::default(),
        mayor_tx,
        mayor_initial,
    );

    let dead_count = Arc::new(AtomicUsize::new(0));
    let (events_tx, _) = broadcast::channel::<EventRecord>(config.event_buffer.max(1));

    let keychain = config.keychain.clone();
    let mut reactor = Reactor {
        sched: sched.clone(),
        patrol: patrol.clone(),
        merge: merge.clone(),
        deacon: deacon.clone(),
        witness: witness.clone(),
        witness_threshold: config.witness_threshold_secs,
        repo: repo.clone(),
        effects,
        clock,
        writer: JsonlWriter::new(&log_path),
        prio: HashMap::new(),
        dead: Vec::new(),
        dead_count: dead_count.clone(),
        events: events_tx.clone(),
        keychain: keychain.clone(),
        notifier: config.notifier.clone(),
    };

    let mut sched_rx = sched_rx;
    let mut patrol_rx = patrol_rx;
    let mut merge_rx = merge_rx;
    let mut orch_rx = orch_rx;
    let mut quota_rx = quota_rx;
    let mut rig_rx = rig_rx;
    let mut agent_rx = agent_rx;
    let mut sheriff_rx = sheriff_rx;
    let mut deacon_rx = deacon_rx;
    let mut refinery_rx = refinery_rx;
    let mut witness_rx = witness_rx;
    let mut mayor_rx = mayor_rx;

    let join = tokio::spawn(async move {
        loop {
            tokio::select! {
                Some(env) = sched_rx.recv() => reactor.ingest(env).await,
                Some(env) = patrol_rx.recv() => reactor.ingest(env).await,
                Some(env) = merge_rx.recv() => reactor.ingest(env).await,
                Some(env) = orch_rx.recv() => reactor.ingest(env).await,
                Some(env) = quota_rx.recv() => reactor.ingest(env).await,
                Some(env) = rig_rx.recv() => reactor.ingest(env).await,
                Some(env) = agent_rx.recv() => reactor.ingest(env).await,
                Some(env) = sheriff_rx.recv() => reactor.ingest(env).await,
                Some(env) = deacon_rx.recv() => reactor.ingest(env).await,
                Some(env) = refinery_rx.recv() => reactor.ingest(env).await,
                Some(env) = witness_rx.recv() => reactor.ingest(env).await,
                Some(env) = mayor_rx.recv() => reactor.ingest(env).await,
                else => break,
            }
        }
    });

    RootHandle {
        sched,
        patrol,
        merge,
        orch,
        quota,
        rig,
        agent,
        sheriff,
        deacon,
        refinery,
        witness,
        mayor,
        agent_events: agent_tx,
        repo,
        log_path,
        dead_count,
        events: events_tx,
        keychain,
        join,
    }
}

/// The single-task state the loop carries: actor handles to react with, the injected edges,
/// the log writer, the per-bead priority cache (so an expired lease re-enqueues at the right
/// priority — `SchedEvent::Dispatched` doesn't carry it), and the dead-letter sink.
struct Reactor<R, FX, CK> {
    sched: SchedHandle,
    patrol: PatrolHandle,
    merge: MergeHandle,
    /// hq-mc72.12 C2 Deacon — track in-flight merges and drive `DrainComplete` on SIGTERM.
    /// Merge::Ready arms `deacon.track`; Merge::Merged / Failed arm `deacon.finish`.
    deacon: DeaconHandle,
    /// hq-mc72.12 C2 Witness — auto-watch workers whose patrol lease expired so the next
    /// tick past `witness_threshold` raises an escalation through the hq-mysw pipeline.
    witness: WitnessHandle,
    witness_threshold: u64,
    repo: R,
    effects: FX,
    clock: CK,
    writer: JsonlWriter,
    prio: HashMap<String, u8>,
    /// In-process dead-letter, kept as a plain owned `Vec` (the loop is a single task, so no
    /// `RefCell`/`Sync` is needed — unlike the sync `Bus`'s collector). Entries reuse the
    /// kernel's [`DeadLetterEntry`] type so nothing is silently dropped.
    dead: Vec<DeadLetterEntry<GtEvent>>,
    dead_count: Arc<AtomicUsize>,
    /// Live broadcast fan-out. Each appended record is also sent here so SSE subscribers see
    /// it; a send error simply means no listeners — not a failure (it is normal during boot).
    events: broadcast::Sender<EventRecord>,
    /// Credential store the rotation chain flips on `Quota::Rotated`. The reaction is at the
    /// edge so the core stays pure (`docs/06-observability.md`).
    keychain: Arc<dyn Keychain>,
    /// Operator-signal sink (hq-mysw). Escalation / quota-block / merge-stuck reactions build
    /// a [`Notification`], decide via `gt_notify::route` whether it warrants mail, and (for
    /// stuck escalations) also create a durable status bead through `repo`.
    notifier: Arc<dyn Notifier>,
}

impl<R, FX, CK> Reactor<R, FX, CK>
where
    R: BeadRepository + Clone + 'static,
    FX: Effects,
    CK: Clock,
{
    /// Record one domain envelope to the log (keeping its exact type-erased shape) and run
    /// its cross-domain reaction. Logging and reaction failures go to the dead-letter.
    async fn ingest<E>(&mut self, env: Envelope<E>)
    where
        E: EventKind + Serialize + Into<GtEvent>,
    {
        // Bump the per-kind events counter once per ingest — the Prometheus exporter sees the
        // exact same population the audit log records. `record_envelope` also attaches the
        // causal triple to whatever span the producer was in (no-op outside one).
        gt_telemetry::record_envelope(&env);

        match EventRecord::from_envelope(&env) {
            Ok(rec) => {
                if let Err(e) = self.writer.append(&rec) {
                    self.dead_error(env.kind(), e);
                }
                // Fan-out to live subscribers (SSE). Ignored if there are none; the log
                // remains the source of truth and the snapshot endpoints still cover replay.
                // Sheriff also subscribes here through `gt_plugin::spawn_plugin_relay` (hq-evks);
                // its plugin impl in `gt_sheriff` self-filters `sheriff.*` to avoid feedback.
                let _ = self.events.send(rec);
            }
            Err(e) => self.dead_error(env.kind(), e),
        }

        let gt: GtEvent = env.payload.into();
        if let Err(e) = self.react(&gt).await {
            let kind = gt.kind();
            self.dead_error(kind, e);
        }
    }

    fn dead_error(&mut self, kind: &'static str, error: AppError) {
        self.dead.push(DeadLetterEntry::HandlerError { kind, error });
        self.dead_count.fetch_add(1, Ordering::SeqCst);
        gt_telemetry::metrics::observe_dead_letter(kind);
    }

    /// The cross-domain wiring. Each arm is a reaction another step deferred to the root;
    /// observations with no cross-domain effect fall through (their log entry is the point).
    async fn react(&mut self, gt: &GtEvent) -> Result<(), AppError> {
        match gt {
            // Remember the priority a bead was queued at so an expired lease can re-enqueue
            // it correctly (Dispatched carries only bead + worker).
            GtEvent::Sched(SchedEvent::Enqueue { bead, priority }) => {
                self.prio.insert(bead.clone(), *priority);
            }
            // A bead was just claimed: open its lease in the patrol (Paso 6.a). `now_secs` is
            // read here, at the edge, and recorded by the patrol — never in the core.
            GtEvent::Sched(SchedEvent::Dispatched { bead, worker }) => {
                let priority = self.prio.get(bead).copied().unwrap_or(1);
                let now = self.clock.now_secs();
                self.patrol
                    .register(bead.clone(), worker.clone(), priority, now)
                    .await;
            }
            // The patrol's pure detector fired: reclaim the lease in the repo (CAS only wins
            // if the bead is still dispatched + owned by the dead worker) and re-enqueue.
            // hq-mc72.12 C2 Witness: also auto-watch the worker so the next tick past the
            // configured threshold raises an escalation (escalation_raised -> status bead +
            // notification via the existing hq-mysw escalation arm).
            GtEvent::Patrol(PatrolEvent::LeaseExpired {
                bead,
                worker,
                priority,
            }) => {
                if self.repo.cas_release(bead, worker).await? {
                    self.sched.capacity_freed().await;
                    self.sched.enqueue(bead.clone(), *priority).await;
                }
                let now = self.clock.now_secs();
                gt_witness::witness::watch(&self.witness, worker, now, self.witness_threshold)
                    .await?;
            }
            // Convoy handoff (Paso 6.d): the next member is ready — sling it.
            GtEvent::Orch(OrchEvent::MemberDispatched { convoy, member }) => {
                self.effects.sling(convoy, member);
            }
            // Refinery observed MERGE_READY (Paso 6.b): advance the slot to Merging. The real
            // `git merge` is an edge effect that then reports Complete/Fail back to the actor.
            // hq-mc72.12 C2 Deacon: also register the bead as an in-flight merge item so the
            // drain on SIGTERM waits for it to finish.
            GtEvent::Merge(MergeEvent::Ready { bead, .. }) => {
                self.merge.start(bead.clone()).await;
                gt_deacon::deacon::track(&self.deacon, bead, "merge").await?;
            }
            // Merge landed: free the dispatcher capacity FIRST (in-memory, infallible) and
            // then close the bead. hq-mcyc.2: a Dolt I/O error on `repo.get`/`upsert` must not
            // strand the scheduling slot — capacity release runs unconditionally, repo update
            // is best-effort. hq-mc72.12 C2 Deacon: clear the in-flight entry — when draining
            // and pending hits zero, the actor emits `DrainComplete` in the same tick.
            GtEvent::Merge(MergeEvent::Merged { bead, .. }) => {
                self.sched.capacity_freed().await;
                match self.repo.get(bead).await {
                    Ok(Some(mut b)) => {
                        b.status = BeadStatus::Done;
                        if let Err(e) = self.repo.upsert(&b).await {
                            eprintln!(
                                "[gt] merge_merged: bead {bead} status upsert failed (capacity already released): {e}"
                            );
                        }
                    }
                    Ok(None) => {}
                    Err(e) => eprintln!(
                        "[gt] merge_merged: bead {bead} repo lookup failed (capacity already released): {e}"
                    ),
                }
                let now = self.clock.now_secs();
                gt_deacon::deacon::finish(&self.deacon, bead, now).await?;
                // hq-mc72.12 C5: work done — stop supervising its polecat.
                self.effects.release(bead);
            }
            // Predictive rotation (Paso 6.c): the account will block (or just did) — rotate.
            // hq-mysw: also raise the quota-block operator signal (notification-only; rotation
            // already created the corrective action, so no status bead).
            GtEvent::Quota(QuotaEvent::BlockPredicted { account, .. })
            | GtEvent::Quota(QuotaEvent::AccountLimited { account, .. }) => {
                self.effects.rotate(account);
                self.escalate(Signal::QuotaBlock {
                    account: account.clone(),
                })
                .await?;
            }
            // hq-mysw: a fully blocked account. Rotation may already have run upstream; this
            // is the human heads-up that the account went dark.
            GtEvent::Quota(QuotaEvent::Blocked { account, .. }) => {
                self.escalate(Signal::QuotaBlock {
                    account: account.clone(),
                })
                .await?;
            }
            // hq-mysw escalation action: the Witness raised a stuck-worker escalation
            // (`witness.escalation_raised`). Create a status bead + route a notification.
            // This is the "wired via Witness" path the gate exercises with a synthetic Stuck.
            GtEvent::Witness(WitnessEvent::EscalationRaised { worker, age_secs }) => {
                self.escalate(Signal::WorkerStuck {
                    worker: worker.clone(),
                    age_secs: *age_secs,
                })
                .await?;
            }
            // hq-mysw: a merge slot failed — the feed's `TimeoutMissed` (`.failed` terminal)
            // class, surfaced live. Stuck lane → status bead + notification. hq-mc72.12 C2
            // Deacon: a failed merge also terminates the in-flight entry (FinishItem for an
            // unknown id is a no-op so an isolated Failed without a prior Ready is safe).
            GtEvent::Merge(MergeEvent::Failed { bead, reason }) => {
                self.escalate(Signal::MergeStuck {
                    bead: bead.clone(),
                    reason: reason.clone(),
                })
                .await?;
                let now = self.clock.now_secs();
                gt_deacon::deacon::finish(&self.deacon, bead, now).await?;
                // hq-mc72.12 C5: terminal failure — the polecat won't recover this bead; stop
                // supervising its session (a retry is a fresh sling with a new watch).
                self.effects.release(bead);
            }
            // hq-0bko.2 gate: a rotation landed. Flip the keychain's live pointer so the next
            // edge call uses the new account's credentials. Failure here is a real bug — log
            // it, but keep the loop running; the bead is closed but the runtime is still
            // using the previous credential until the operator fixes the keychain.
            GtEvent::Quota(QuotaEvent::Rotated { to_account, .. }) => {
                if let Err(e) = self.keychain.set_active(to_account) {
                    eprintln!(
                        "[gt] keychain: set_active({to_account}) failed: {e} — credential pointer NOT flipped"
                    );
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// hq-mysw escalation action. Builds the canonical [`Notification`] for `signal`, creates a
    /// durable status bead for stuck escalations (so the gap survives a restart and shows in
    /// the bead queue), and — when `gt_notify::route` says the signal warrants mail — pushes it
    /// through the injected [`Notifier`]. Quota-block is notification-only (no bead): rotation
    /// already created the corrective action. A status-bead write failure dead-letters via the
    /// `?`; the notification itself is best-effort and never blocks the loop.
    async fn escalate(&mut self, signal: Signal) -> Result<(), AppError> {
        if let Some(bead) = escalation_bead(&signal) {
            self.repo.upsert(&bead).await?;
        }
        if gt_notify::route(&signal) == Channel::Mail {
            self.notifier.notify(&Notification::for_signal(signal));
        }
        Ok(())
    }
}

/// The status bead an escalation leaves behind, or `None` for notification-only signals.
///
/// Created as [`BeadStatus::Failed`], priority 0 (P0): `Failed` keeps the scheduler from
/// dispatching the alert as if it were work (only `Pending` beads are claimable), while P0
/// surfaces it at the top of any operator panel. The id is deterministic per subject so a
/// repeated escalation upserts the same row instead of piling up duplicates.
fn escalation_bead(signal: &Signal) -> Option<Bead> {
    match signal {
        Signal::WorkerStuck { worker, age_secs } => Some(Bead::new(
            format!("escalation-{worker}"),
            format!("Escalation: worker {worker} stuck ({age_secs}s past threshold)"),
            BeadStatus::Failed,
            0,
        )),
        Signal::MergeStuck { bead, reason } => Some(Bead::new(
            format!("escalation-merge-{bead}"),
            format!("Escalation: merge {bead} stuck ({reason})"),
            BeadStatus::Failed,
            0,
        )),
        // Quota-block self-heals via rotation; the notification is the heads-up, no bead.
        Signal::QuotaBlock { .. } => None,
    }
}
