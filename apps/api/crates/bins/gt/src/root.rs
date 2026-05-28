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

use gt_audit::{EventRecord, EventStore, JsonlWriter};
use gt_beads::{BeadRepository, BeadStatus};
use gt_bus::DeadLetterEntry;
use gt_events::{AppError, Envelope, EventKind};

use gt_agent::actor::{self as agent_actor, AgentHandle};
use gt_agent::AgentEvent;
use gt_merge::actor::{self as merge_actor, MergeHandle};
use gt_merge::MergeEvent;
use gt_orchestration::actor::{self as orch_actor, OrchHandle};
use gt_orchestration::OrchEvent;
use gt_patrol::actor::{self as patrol_actor, PatrolHandle};
use gt_patrol::PatrolEvent;
use gt_quota::actor::{self as quota_actor, QuotaHandle};
use gt_quota::{InMemoryKeychain, Keychain, ModelWeights, QuotaEvent};
use gt_scheduling::actor::{self as sched_actor, SchedHandle};
use gt_scheduling::SchedEvent;

use crate::event::GtEvent;

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

/// Handle to a running root. Holds clones of every actor handle plus the agent relay so the
/// edge (timers, probes, the channel watcher, the test) can drive the system; the internal
/// loop observes what the actors emit, logs it, and reacts.
pub struct RootHandle<R: BeadRepository + Clone> {
    pub sched: SchedHandle,
    pub patrol: PatrolHandle,
    pub merge: MergeHandle,
    pub orch: OrchHandle,
    pub quota: QuotaHandle,
    pub agent: AgentHandle,
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
}

impl Default for RootConfig {
    fn default() -> Self {
        Self {
            capacity: 4,
            model_weights: HashMap::new(),
            event_buffer: 1024,
            keychain: Arc::new(InMemoryKeychain::new()),
        }
    }
}

/// Spawn the whole system: actors + the draining loop. `repo` is the Dolt-port (or its
/// in-memory stand-in), `effects`/`clock` are the injected edges, `log_path` is the single
/// audit log.
pub fn spawn<R, FX, CK>(
    repo: R,
    effects: FX,
    clock: CK,
    log_path: impl Into<PathBuf>,
    config: RootConfig,
) -> RootHandle<R>
where
    R: BeadRepository + Clone + 'static,
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
    let (agent_tx, agent_rx) = mpsc::channel::<Envelope<AgentEvent>>(256);

    let sched = sched_actor::spawn(repo.clone(), sched_tx, config.capacity);
    let patrol = patrol_actor::spawn(patrol_tx);
    let merge = merge_actor::spawn(merge_tx);
    let orch = orch_actor::spawn(orch_tx);
    let quota = quota_actor::spawn(quota_tx, config.model_weights);
    let agent = agent_actor::spawn(256);

    let dead_count = Arc::new(AtomicUsize::new(0));
    let (events_tx, _) = broadcast::channel::<EventRecord>(config.event_buffer.max(1));

    let keychain = config.keychain.clone();
    let mut reactor = Reactor {
        sched: sched.clone(),
        patrol: patrol.clone(),
        merge: merge.clone(),
        repo: repo.clone(),
        effects,
        clock,
        writer: JsonlWriter::new(&log_path),
        prio: HashMap::new(),
        dead: Vec::new(),
        dead_count: dead_count.clone(),
        events: events_tx.clone(),
        keychain: keychain.clone(),
    };

    let mut sched_rx = sched_rx;
    let mut patrol_rx = patrol_rx;
    let mut merge_rx = merge_rx;
    let mut orch_rx = orch_rx;
    let mut quota_rx = quota_rx;
    let mut agent_rx = agent_rx;

    let join = tokio::spawn(async move {
        loop {
            tokio::select! {
                Some(env) = sched_rx.recv() => reactor.ingest(env).await,
                Some(env) = patrol_rx.recv() => reactor.ingest(env).await,
                Some(env) = merge_rx.recv() => reactor.ingest(env).await,
                Some(env) = orch_rx.recv() => reactor.ingest(env).await,
                Some(env) = quota_rx.recv() => reactor.ingest(env).await,
                Some(env) = agent_rx.recv() => reactor.ingest(env).await,
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
        agent,
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
            GtEvent::Patrol(PatrolEvent::LeaseExpired {
                bead,
                worker,
                priority,
            }) => {
                if self.repo.cas_release(bead, worker).await? {
                    self.sched.capacity_freed().await;
                    self.sched.enqueue(bead.clone(), *priority).await;
                }
            }
            // Convoy handoff (Paso 6.d): the next member is ready — sling it.
            GtEvent::Orch(OrchEvent::MemberDispatched { convoy, member }) => {
                self.effects.sling(convoy, member);
            }
            // Refinery observed MERGE_READY (Paso 6.b): advance the slot to Merging. The real
            // `git merge` is an edge effect that then reports Complete/Fail back to the actor.
            GtEvent::Merge(MergeEvent::Ready { bead, .. }) => {
                self.merge.start(bead.clone()).await;
            }
            // Merge landed: close the bead and free the capacity it held (the cross-domain
            // integration gt-merge deliberately left to the root).
            GtEvent::Merge(MergeEvent::Merged { bead, .. }) => {
                if let Some(mut b) = self.repo.get(bead).await? {
                    b.status = BeadStatus::Done;
                    self.repo.upsert(&b).await?;
                }
                self.sched.capacity_freed().await;
            }
            // Predictive rotation (Paso 6.c): the account will block (or just did) — rotate.
            GtEvent::Quota(QuotaEvent::BlockPredicted { account, .. })
            | GtEvent::Quota(QuotaEvent::AccountLimited { account, .. }) => {
                self.effects.rotate(account);
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
}
