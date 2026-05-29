//! `GtEvent` / `GtState` — the unifier the composition root needs.
//!
//! Each domain ships an **owned** event enum and a pure reducer (`docs/03-events.md`). They
//! never reference each other. The root is the one place allowed to know all of them, so it
//! is also the one place that can offer a single type spanning the whole system:
//!
//! - [`GtEvent`] is the sum of every domain event. Its [`EventKind::kind`] **delegates** to
//!   the inner event, so the routing key on the wire is unchanged (`scheduling.dispatched`,
//!   not `gt.scheduling…`). That is deliberate: the audit log keeps the exact per-domain,
//!   type-erased shape it already has, so the per-domain prefix replay from Pasos 2–6 still
//!   works byte-for-byte. `GtEvent` is an in-process routing/aggregation type, **not** a new
//!   wire format — the root records each domain envelope directly (`EventRecord::from_envelope`).
//! - [`GtState`] is the aggregate of every domain's reducer state. [`GtState::apply`] folds a
//!   `GtEvent` into the matching sub-state. [`replay_gt`] rebuilds the whole system from the
//!   single log; because each sub-state is independent and each domain's events keep their
//!   relative order, the result is deterministic regardless of cross-domain interleaving
//!   (the determinism rule, `docs/06-observability.md`).

use gt_audit::EventRecord;
use gt_events::{AppError, EventKind};

use gt_agent::{AgentEvent, SessionRegistry};
use gt_feed::{Curator, FeedState};
use gt_mayor::{MayorEvent, MayorState};
use gt_merge::{MergeEvent, MergeState};
use gt_orchestration::{OrchEvent, OrchState};
use gt_patrol::{PatrolEvent, PatrolState};
use gt_quota::{QuotaEvent, QuotaState};
use gt_scheduling::{SchedEvent, SchedState};
use gt_sheriff::{SheriffEvent, SheriffState};
use gt_deacon::{DeaconEvent, DeaconState};
use gt_refinery::{RefineryEvent, RefineryState};
use gt_witness::{WitnessEvent, WitnessState};

/// The sum of every domain event. Owned (no lifetimes, trivially `Send`); the `kind()`
/// match is exhaustive, so adding a domain forces a new arm here — the compiler enforces
/// that the root cannot silently ignore a new event family.
#[derive(Debug, Clone, PartialEq)]
pub enum GtEvent {
    Agent(AgentEvent),
    Sched(SchedEvent),
    Patrol(PatrolEvent),
    Merge(MergeEvent),
    Quota(QuotaEvent),
    Orch(OrchEvent),
    Sheriff(SheriffEvent),
    Deacon(DeaconEvent),
    Refinery(RefineryEvent),
    Witness(WitnessEvent),
    Mayor(MayorEvent),
}

impl EventKind for GtEvent {
    /// Delegates to the inner event so the bus routing key — and the `type` field in the
    /// log — is identical to the per-domain one. No `gt.` prefix is added.
    fn kind(&self) -> &'static str {
        match self {
            GtEvent::Agent(e) => e.kind(),
            GtEvent::Sched(e) => e.kind(),
            GtEvent::Patrol(e) => e.kind(),
            GtEvent::Merge(e) => e.kind(),
            GtEvent::Quota(e) => e.kind(),
            GtEvent::Orch(e) => e.kind(),
            GtEvent::Sheriff(e) => e.kind(),
            GtEvent::Deacon(e) => e.kind(),
            GtEvent::Refinery(e) => e.kind(),
            GtEvent::Witness(e) => e.kind(),
            GtEvent::Mayor(e) => e.kind(),
        }
    }
}

macro_rules! gt_from {
    ($variant:ident, $ty:ty) => {
        impl From<$ty> for GtEvent {
            fn from(e: $ty) -> Self {
                GtEvent::$variant(e)
            }
        }
    };
}
gt_from!(Agent, AgentEvent);
gt_from!(Sched, SchedEvent);
gt_from!(Patrol, PatrolEvent);
gt_from!(Merge, MergeEvent);
gt_from!(Quota, QuotaEvent);
gt_from!(Orch, OrchEvent);
gt_from!(Sheriff, SheriffEvent);
gt_from!(Deacon, DeaconEvent);
gt_from!(Refinery, RefineryEvent);
gt_from!(Witness, WitnessEvent);
gt_from!(Mayor, MayorEvent);

/// Wire prefixes that ride in the event log but are **not** domain state: frontier-audit
/// observability (e.g. `mcp.invoked` from `gt-mcp`). Domain replay skips them so reconstructed
/// state stays byte-identical, while the feed still folds them (it is prefix-agnostic). Kept
/// strict otherwise — a genuinely unknown prefix is still an error (`GtEvent::from_record`),
/// which is what catches a mis-wired domain event.
const META_PREFIXES: &[&str] = &["mcp"];

/// Whether a record is a meta (non-domain) event that domain replay must skip.
pub fn is_meta(rec: &EventRecord) -> bool {
    rec.kind
        .split_once('.')
        .is_some_and(|(domain, _)| META_PREFIXES.contains(&domain))
}

impl GtEvent {
    /// Decode a type-erased [`EventRecord`] back into the typed `GtEvent`, routed by the
    /// domain prefix of its `kind` (`"scheduling."`, `"patrol."`, …). This is the inverse of
    /// recording a domain envelope: the root writes the inner payload, replay reads it back
    /// into the right variant. An unknown prefix is an error, not a silent drop.
    pub fn from_record(rec: &EventRecord) -> Result<Self, AppError> {
        let domain = rec
            .kind
            .split_once('.')
            .map(|(d, _)| d)
            .ok_or_else(|| AppError::Other(format!("event kind without domain prefix: {}", rec.kind)))?;
        Ok(match domain {
            "agent" => GtEvent::Agent(rec.decode()?),
            "scheduling" => GtEvent::Sched(rec.decode()?),
            "patrol" => GtEvent::Patrol(rec.decode()?),
            "merge" => GtEvent::Merge(rec.decode()?),
            "quota" => GtEvent::Quota(rec.decode()?),
            "orch" => GtEvent::Orch(rec.decode()?),
            "sheriff" => GtEvent::Sheriff(rec.decode()?),
            "deacon" => GtEvent::Deacon(rec.decode()?),
            "refinery" => GtEvent::Refinery(rec.decode()?),
            "witness" => GtEvent::Witness(rec.decode()?),
            "mayor" => GtEvent::Mayor(rec.decode()?),
            other => return Err(AppError::Other(format!("unknown event domain: {other}"))),
        })
    }
}

/// The aggregate state of the whole system: one field per domain reducer. Each field is
/// rebuilt only from its own domain's events (folded by [`GtState::apply`]); the domains
/// stay isolated even inside the unifier.
///
/// `feed` is the type-erased projection (`gt-feed`, Paso 6.g): a pure consumer of the log,
/// independent of every domain reducer. It is rebuilt by [`replay_gt`] from the raw
/// `EventRecord`s, not from `GtEvent` — that keeps `gt-feed` decoupled from the typed enum
/// and lets it observe events whose domain prefix is unknown to the unifier.
#[derive(Debug, Default)]
pub struct GtState {
    pub agent: SessionRegistry,
    pub sched: SchedState,
    pub patrol: PatrolState,
    pub merge: MergeState,
    pub quota: QuotaState,
    pub orch: OrchState,
    pub sheriff: SheriffState,
    pub deacon: DeaconState,
    pub refinery: RefineryState,
    pub witness: WitnessState,
    pub mayor: MayorState,
    pub feed: FeedState,
}

impl GtState {
    /// Fold one unified event into the matching sub-state. Pure: no clock, no random — every
    /// time-derived number already travelled in the event payload from the edge.
    pub fn apply(&mut self, event: &GtEvent) {
        match event {
            GtEvent::Agent(e) => self.agent.apply(e),
            GtEvent::Sched(e) => self.sched.apply(e),
            GtEvent::Patrol(e) => self.patrol.apply(e),
            GtEvent::Merge(e) => self.merge.apply(e),
            GtEvent::Quota(e) => self.quota.apply(e),
            GtEvent::Orch(e) => self.orch.apply(e),
            // SheriffState::apply returns Result for symmetry with the actor's apply, but
            // the fold here is total — every reducer is. Ignoring the Err matches the other
            // reducers' signatures (they return ()).
            GtEvent::Sheriff(e) => {
                let _ = self.sheriff.apply(e);
            }
            GtEvent::Deacon(e) => {
                let _ = self.deacon.apply(e);
            }
            GtEvent::Refinery(e) => {
                let _ = self.refinery.apply(e);
            }
            // WitnessState::apply mirrors the SheriffState shape (Result for symmetry; the
            // fold is total in practice).
            GtEvent::Witness(e) => {
                let _ = self.witness.apply(e);
            }
            // Same pattern as Sheriff: the typed apply returns Result for actor symmetry,
            // but the reducer here is total — fold and discard.
            GtEvent::Mayor(e) => {
                let _ = self.mayor.apply(e);
            }
        }
    }
}

/// Rebuild the entire system state from the single audit log. Decodes each record into a
/// `GtEvent` by its domain prefix and folds it. Same input → same `GtState`, byte-for-byte:
/// the Paso 3 determinism gate, now across all domains at once.
///
/// The feed projection is folded in the same loop straight from the raw record — it is the
/// type-erased view the TUI consumes, and folding it here makes the Paso 6.g gate ride along
/// with the existing replay: same log → same `GtState.feed`.
pub fn replay_gt(records: &[EventRecord]) -> Result<GtState, AppError> {
    let mut state = GtState::default();
    for rec in records {
        // The feed is prefix-agnostic: it folds every record, including meta/frontier-audit.
        Curator::apply(&mut state.feed, rec);
        // Meta events (`mcp.*`) carry no domain state — skip them so domain replay stays
        // byte-identical whether or not frontier audit shares the log.
        if is_meta(rec) {
            continue;
        }
        let event = GtEvent::from_record(rec)?;
        state.apply(&event);
    }
    Ok(state)
}
