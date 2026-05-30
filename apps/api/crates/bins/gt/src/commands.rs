//! `CommandBus` — single dispatcher for every domain `Command` (hq-fe-api-w.1).
//!
//! Pre-bus, each frontier (gt-mcp tools, future gt-web routes, gt CLI) reimplemented the
//! same sequence per domain: pick the right actor handle, call `validate` or `exec`, fold
//! the result. The bus collapses that to a single match over `RootCommand`, so frontiers
//! only own their boundary concerns (auth/scope, audit, transport encoding).
//!
//! The bus is intentionally side-effect free besides the actor dispatch — scope
//! authorization and audit logging live at the frontier (where the *actor* is known and
//! the tool-name string lives). That keeps the domain handles unaware of frontier policy
//! and lets the same bus serve MCP, HTTP, and CLI without compromise.
//!
//! Idempotency (`idem_key`) is accepted as an opaque hint today and ignored by the
//! dispatcher; hq-fe-api-w.2 ships the keyed cache middleware that consumes it. Threading
//! the parameter through now means downstream frontiers can already pass it.

use gt_agent::{actor::AgentHandle, AgentCommand};
use gt_events::AppError;
use gt_merge::{actor::MergeHandle, MergeCommand};
use gt_orchestration::{actor::OrchHandle, OrchCommand};
use gt_patrol::{actor::PatrolHandle, PatrolCommand};
use gt_quota::{actor::QuotaHandle, QuotaCommand};
use gt_rig::{actor::RigHandle, RigCommand};
use gt_scheduling::{actor::SchedHandle, SchedCommand};

/// Tagged union of every domain command the bus can route. Each variant carries the
/// existing per-domain `*Command` enum verbatim so the actors keep their current public
/// surface — the bus is a routing layer, not a new domain.
#[derive(Debug, Clone)]
pub enum RootCommand {
    Agent(AgentCommand),
    Merge(MergeCommand),
    Sched(SchedCommand),
    Patrol(PatrolCommand),
    Orch(OrchCommand),
    Quota(QuotaCommand),
    Rig(RigCommand),
}

impl RootCommand {
    /// Stable name used by the frontier when recording audit entries / metrics. Mirrors
    /// the dot-separated tool families gt-mcp already exposes (`agent.add`, `merge.submit`,
    /// ...). Frontiers that want the full tool name (`agent.add.execute`) build it from
    /// this stem plus the validate/execute suffix.
    pub fn domain(&self) -> &'static str {
        match self {
            Self::Agent(_) => "agent",
            Self::Merge(_) => "merge",
            Self::Sched(_) => "scheduling",
            Self::Patrol(_) => "patrol",
            Self::Orch(_) => "orch",
            Self::Quota(_) => "quota",
            Self::Rig(_) => "rig",
        }
    }
}

/// The bus itself. Clone-cheap (every handle is an `Arc`/mpsc clone), so frontiers can
/// stash it in their own service struct without lifetime gymnastics.
///
/// `rig` is `Option` to match gt-mcp's current pre-wired-rig contract: a `None` rig
/// returns `AppError::Other("rig domain not wired")` on `Rig` commands, leaving the rest
/// of the surface usable. Once every composition root spawns the rig actor (the gt-rs
/// root already does), this can tighten to a required handle.
#[derive(Clone)]
pub struct CommandBus {
    agent: AgentHandle,
    merge: MergeHandle,
    sched: SchedHandle,
    patrol: PatrolHandle,
    orch: OrchHandle,
    quota: QuotaHandle,
    rig: Option<RigHandle>,
}

impl CommandBus {
    /// Build the bus from raw domain handles. The rig domain is opt-in via
    /// [`CommandBus::with_rig`] so tests that do not need `rig.*` keep a single-call ctor.
    pub fn new(
        agent: AgentHandle,
        merge: MergeHandle,
        sched: SchedHandle,
        patrol: PatrolHandle,
        orch: OrchHandle,
        quota: QuotaHandle,
    ) -> Self {
        Self {
            agent,
            merge,
            sched,
            patrol,
            orch,
            quota,
            rig: None,
        }
    }

    /// Builder-style setter for the rig catalog handle. Returns a fresh bus that shares
    /// every other handle.
    pub fn with_rig(mut self, rig: RigHandle) -> Self {
        self.rig = Some(rig);
        self
    }

    /// Validate `cmd` against the domain's current state without applying it. Pure read
    /// over the actor snapshot — no events emitted, no state changes. The actor revalidates
    /// on `dispatch` to close the TOCTOU window the design contract spells out.
    ///
    /// `idem_key` is accepted for forward compatibility with hq-fe-api-w.2 and currently
    /// ignored at this layer; callers that already have one should pass it through.
    pub async fn validate(
        &self,
        cmd: &RootCommand,
        _idem_key: Option<&str>,
    ) -> Result<(), AppError> {
        match cmd {
            RootCommand::Agent(c) => self.agent.validate(c.clone()).await,
            RootCommand::Merge(c) => self.merge.validate(c.clone()).await,
            RootCommand::Sched(c) => self.sched.validate(c.clone()).await,
            RootCommand::Patrol(c) => self.patrol.validate(c.clone()).await,
            RootCommand::Orch(c) => self.orch.validate(c.clone()).await,
            RootCommand::Quota(c) => self.quota.validate(c.clone()).await,
            RootCommand::Rig(c) => match &self.rig {
                Some(rig) => rig.validate(c.clone()).await,
                None => Err(AppError::Other("rig domain not wired".into())),
            },
        }
    }

    /// Apply `cmd`. The actor revalidates before mutating, so a stale `validate` snapshot
    /// cannot desync state. `idem_key` is consumed as in [`Self::validate`].
    pub async fn dispatch(
        &self,
        cmd: RootCommand,
        _idem_key: Option<&str>,
    ) -> Result<(), AppError> {
        match cmd {
            RootCommand::Agent(c) => self.agent.exec(c).await,
            RootCommand::Merge(c) => self.merge.exec(c).await,
            RootCommand::Sched(c) => self.sched.exec(c).await,
            RootCommand::Patrol(c) => self.patrol.exec(c).await,
            RootCommand::Orch(c) => self.orch.exec(c).await,
            RootCommand::Quota(c) => self.quota.exec(c).await,
            RootCommand::Rig(c) => match &self.rig {
                Some(rig) => rig.exec(c).await,
                None => Err(AppError::Other("rig domain not wired".into())),
            },
        }
    }

    pub fn agent(&self) -> &AgentHandle {
        &self.agent
    }

    pub fn merge(&self) -> &MergeHandle {
        &self.merge
    }

    pub fn sched(&self) -> &SchedHandle {
        &self.sched
    }

    pub fn patrol(&self) -> &PatrolHandle {
        &self.patrol
    }

    pub fn orch(&self) -> &OrchHandle {
        &self.orch
    }

    pub fn quota(&self) -> &QuotaHandle {
        &self.quota
    }

    pub fn rig(&self) -> Option<&RigHandle> {
        self.rig.as_ref()
    }
}
