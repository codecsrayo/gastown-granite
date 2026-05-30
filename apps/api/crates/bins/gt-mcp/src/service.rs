//! `McpService` — the rmcp tool router for `gt-mcp`.
//!
//! Each domain `Command` is exposed as two MCP tools: `<base>.validate` and
//! `<base>.execute`. The macro-generated router dispatches by wire name; both routes
//! converge on [`McpService::run`], which performs scope authorization, drives the
//! actor through `validate` or `exec`, and records the audit envelope. The actor
//! revalidates on `exec` inside the same tick, so a stale `validate` snapshot cannot
//! desync state.
//!
//! Tests drive `run` directly — same code path as the macro-generated tools, but
//! without depending on the wire transport. The transport is exercised by `main.rs`
//! via `rmcp::transport::stdio`.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc;

use rmcp::{
    handler::server::wrapper::Parameters,
    model::{
        AnnotateAble, CallToolResult, Content, ErrorData as McpError, Implementation,
        ListResourcesResult, PaginatedRequestParams, RawResource, ReadResourceRequestParams,
        ReadResourceResult, ResourceContents, ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};

use gt_events::{AppError, Envelope};

use gt_root::{CommandBus, RootCommand};
use gt_agent::actor::AgentHandle;
use gt_agent::{
    AddSession, AgentCommand, AgentEvent, RemoveSession, Session, SessionQueries, SessionRole,
    TransitionSession,
};
use gt_beads::{Bead, BeadStatus};
use gt_store_dolt::{DoltIssues, DoltSessions, IssueFilter};
use gt_merge::{CompleteMerge, FailMerge, MergeCommand, SubmitMerge};
use gt_orchestration::{CompleteMember, FailMember, LaunchConvoy, OrchCommand};
use gt_patrol::{CloseLease, Heartbeat, PatrolCommand, RegisterLease, Tick};
use gt_scheduling::{Enqueue, MarkDispatched, SchedCommand};
use gt_quota::{
    Account, AccountQuotaStatus, AccountWindow, ProbeWindow, QuotaCommand, RotateAccount,
    SampleTokens, WindowKind,
};
use gt_rig::actor::RigHandle;
use gt_rig::{
    AddRig, AdoptRig, RemoveRig, RigCommand, SetRigDefaultBranch, SetRigPrefix,
};

use crate::audit::{AuditEvent, AuditSink, Outcome};
use crate::auth::Scope;

/// Read-side backend for the `gt://agent/sessions` resource. The actor variant keeps the
/// historical behavior (in-memory snapshot from `AgentHandle`); the Dolt variant reads the
/// canonical `sessions` table — the same one polecats write via `gt sling` (Paso 6.h epic A,
/// hq-u955). Enum dispatch keeps the non-`dyn` rule of `docs/01-architecture.md` intact while
/// allowing tests to keep the actor backend without changing their constructor calls.
#[derive(Clone)]
pub enum SessionsRead {
    Actor(AgentHandle),
    Dolt(Arc<DoltSessions>),
}

impl SessionsRead {
    pub async fn snapshot(&self) -> Vec<Session> {
        match self {
            Self::Actor(h) => h.snapshot().await,
            Self::Dolt(d) => match d.active_sessions().await {
                Ok(rows) => rows,
                Err(e) => {
                    eprintln!("[gt-mcp] dolt sessions read failed: {e}");
                    Vec::new()
                }
            },
        }
    }
}

/// Read-side backend for the `gt://issues` resource (hq-mcp-issues.1). Unlike
/// [`SessionsRead`] there is no in-memory actor for the canonical `issues` table
/// — it is owned by `bd` / Dolt and the MCP boundary only reads it. `None`
/// means the gt-mcp boot did not get a Dolt URL (in-memory dev runs); the
/// resource then returns an empty JSON array, matching the early-return shape
/// `gt://rigs` uses before its actor is wired.
#[derive(Clone, Default)]
pub struct IssuesRead {
    inner: Option<Arc<DoltIssues>>,
}

impl IssuesRead {
    pub fn dolt(d: Arc<DoltIssues>) -> Self {
        Self { inner: Some(d) }
    }

    pub fn none() -> Self {
        Self { inner: None }
    }

    pub async fn snapshot(
        &self,
        filter: &IssueFilter,
    ) -> Result<serde_json::Value, AppError> {
        match &self.inner {
            Some(d) => {
                let rows = d.list(filter).await?;
                serde_json::to_value(&rows)
                    .map_err(|e| AppError::Other(format!("encode issues: {e}")))
            }
            None => Ok(serde_json::Value::Array(Vec::new())),
        }
    }
}

/// Input for the `quota.register` tool (hq-mc72.10). Account registration is intentionally
/// *not* a `QuotaCommand`: the domain treats window initialization as edge configuration that
/// arrives outside the event log (`gt-quota::state` — the edge re-registers on boot, the actor
/// re-derives rates from the next probe). So this DTO lives at the edge and feeds
/// `QuotaHandle::upsert_account` directly. Without it, sample/probe/rotate are no-ops over an
/// empty registry, since they only mutate accounts that already exist.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct RegisterAccount {
    /// Account id (the provider / keychain correlative). Must be non-empty.
    pub account: String,
    /// Budget for the live window, in cost units. Must be > 0.
    pub limit: u64,
    /// Window start (UTC epoch seconds).
    pub started_at_secs: u64,
    /// When the window resets (UTC epoch seconds). Must be after `started_at_secs`.
    pub resets_at_secs: u64,
    /// Use the weekly window instead of the default rolling-5h.
    #[serde(default)]
    pub weekly: bool,
}

impl RegisterAccount {
    fn validate(&self) -> Result<(), AppError> {
        if self.account.is_empty() {
            return Err(AppError::Validation("account is empty".into()));
        }
        if self.limit == 0 {
            return Err(AppError::Validation("limit must be > 0".into()));
        }
        if self.resets_at_secs <= self.started_at_secs {
            return Err(AppError::Validation(
                "resets_at_secs must be after started_at_secs".into(),
            ));
        }
        Ok(())
    }

    fn to_account(&self) -> Account {
        Account {
            id: self.account.clone(),
            status: AccountQuotaStatus::Healthy,
            window: Some(AccountWindow {
                kind: if self.weekly {
                    WindowKind::Weekly
                } else {
                    WindowKind::Rolling5h
                },
                limit: self.limit,
                started_at_secs: self.started_at_secs,
                resets_at_secs: self.resets_at_secs,
                consumed: 0.0,
            }),
        }
    }
}

/// Input for the `quota.retire` tool (hq-mc72.12.25). Symmetric edge op to
/// [`RegisterAccount`]: removes an account from the registry. Like register, it is **not** a
/// `QuotaCommand` — there is no domain event. The predictive machinery (per-account rate EWMA,
/// remaining-window probes) naturally goes quiet for the id once no more samples reach it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct RetireAccount {
    /// Account id to drop from the registry. Must be non-empty.
    pub account: String,
}

impl RetireAccount {
    fn validate(&self) -> Result<(), AppError> {
        if self.account.is_empty() {
            return Err(AppError::Validation("account is empty".into()));
        }
        Ok(())
    }
}

/// Input for the `scheduling.create_bead` tool (hq-mc72.10). Mints a `pending` bead in the
/// repo so the dispatcher's CAS-claim has work to find — closing the loop the MCP surface
/// otherwise couldn't drive (`scheduling.enqueue` dispatches nothing if no `pending` bead
/// exists). Routed through the scheduling actor, the one task that owns the `BeadRepository`
/// handle; in production beads still originate in Dolt/`bd`, this is the edge equivalent.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct CreateBead {
    /// Unique bead id. Must be non-empty.
    pub id: String,
    /// Human-readable title for the bead.
    pub title: String,
    /// Priority: 0 = P0 (highest) .. 2 = P2.
    pub priority: u8,
}

impl CreateBead {
    fn validate(&self) -> Result<(), AppError> {
        if self.id.is_empty() {
            return Err(AppError::Validation("bead id is empty".into()));
        }
        Ok(())
    }

    fn to_bead(&self) -> Bead {
        Bead::new(&self.id, &self.title, BeadStatus::Pending, self.priority)
    }
}

/// Input for the `meta.report_gap` tool (hq-mcp-onboard.8). Closes the agent loop the
/// gap-discipline doc section §4 prescribes: when a tool an agent needs does not exist,
/// the agent calls this with the missing operation's canonical name; the server mints a
/// `hq-gap-…` bead the routine catalog can pick up. Bead id + title are derived from
/// `operation` so two agents reporting the same gap in the same second hit the same id —
/// idempotency is not enforced at the actor (CreateBead is upsert today), but the slug
/// keeps duplicates obvious.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ReportGap {
    /// Canonical name of the missing operation, e.g. `issues.update.execute`. Required.
    pub operation: String,
    /// Optional free-form context: what payload was expected, why blocked, links.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Optional priority override (0 = P0 .. 2 = P2). Defaults to P2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,
}

impl ReportGap {
    fn validate(&self) -> Result<(), AppError> {
        if self.operation.trim().is_empty() {
            return Err(AppError::Validation("operation is empty".into()));
        }
        Ok(())
    }

    /// Derive a stable, filesystem-safe bead id from the operation name plus a coarse
    /// timestamp suffix so concurrent reports of the same gap do not collide on the
    /// upsert. Format: `hq-gap-<sanitized-operation>-<unix-secs>`.
    fn to_bead_id(&self) -> String {
        let slug: String = self
            .operation
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
            .collect();
        let slug = slug.trim_matches('-').to_string();
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        format!("hq-gap-{slug}-{secs}")
    }

    fn to_title(&self) -> String {
        match self.notes.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(n) => {
                let truncated: String = n.chars().take(180).collect();
                format!("gap: {} — {}", self.operation, truncated)
            }
            None => format!("gap: {}", self.operation),
        }
    }

    fn effective_priority(&self) -> u8 {
        self.priority.unwrap_or(2)
    }

    fn to_create_bead(&self) -> CreateBead {
        CreateBead {
            id: self.to_bead_id(),
            title: self.to_title(),
            priority: self.effective_priority(),
        }
    }
}

#[derive(Clone)]
pub struct McpService {
    inner: Arc<Inner>,
}

struct Inner {
    /// hq-fe-api-w.1 — the single dispatcher for every domain command. Every `run_*`
    /// helper used to own a sibling actor handle + per-domain match; they now all route
    /// through `bus.validate` / `bus.dispatch` and share the same scope + audit boundary.
    /// The bus carries an `Option<RigHandle>` internally, preserving the previous "rig
    /// domain not wired" behavior for tests that build via [`McpService::new`].
    bus: CommandBus,
    sessions: SessionsRead,
    /// Read-side for the `gt://issues` snapshot (hq-mcp-issues.1). Default is
    /// the [`IssuesRead::none`] variant so the existing test ctors keep one
    /// call; `with_issues` wires the Dolt-backed reader.
    issues: IssuesRead,
    scope: Scope,
    audit: Arc<dyn AuditSink>,
    /// Edge relay for agent events. The agent actor is relay-less by design (the supervisor
    /// and spawn edges emit on it — see `bins/gt::root`), so a tool-driven `agent.add` must
    /// publish `Spawned` here itself for the event to reach the log/broadcast/projector.
    /// `None` in tests built via [`McpService::new`]; `main.rs` wires the root's relay.
    agent_events: Option<mpsc::Sender<Envelope<AgentEvent>>>,
}

#[tool_router]
impl McpService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        agent: AgentHandle,
        merge: gt_merge::actor::MergeHandle,
        sched: gt_scheduling::actor::SchedHandle,
        patrol: gt_patrol::actor::PatrolHandle,
        orch: gt_orchestration::actor::OrchHandle,
        quota: gt_quota::actor::QuotaHandle,
        scope: Scope,
        audit: Arc<dyn AuditSink>,
    ) -> Self {
        let sessions = SessionsRead::Actor(agent.clone());
        let bus = CommandBus::new(agent, merge, sched, patrol, orch, quota);
        Self::from_bus(bus, sessions, scope, audit, None)
    }

    /// Build directly from a [`CommandBus`] — the path the composition root takes via
    /// `RootHandle::commands()` (hq-fe-api-w.1). `with_sessions` is the legacy ctor for
    /// callers that still hand over the individual actor handles.
    pub fn from_bus(
        bus: CommandBus,
        sessions: SessionsRead,
        scope: Scope,
        audit: Arc<dyn AuditSink>,
        agent_events: Option<mpsc::Sender<Envelope<AgentEvent>>>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                bus,
                sessions,
                issues: IssuesRead::none(),
                scope,
                audit,
                agent_events,
            }),
        }
    }

    /// Builder-style setter for the Dolt-backed `gt://issues` snapshot
    /// (hq-mcp-issues.1). Returns a fresh [`McpService`] sharing the same
    /// actors/audit; the composition root chains this after
    /// [`McpService::with_sessions`] when `GT_DOLT_URL` is set. Unset = empty
    /// JSON array, matching the `gt://rigs` early-return shape.
    pub fn with_issues(self, issues: IssuesRead) -> Self {
        let prev = &*self.inner;
        Self {
            inner: Arc::new(Inner {
                bus: prev.bus.clone(),
                sessions: prev.sessions.clone(),
                issues,
                scope: prev.scope.clone(),
                audit: prev.audit.clone(),
                agent_events: prev.agent_events.clone(),
            }),
        }
    }

    /// Same as [`McpService::new`] but lets the caller override the sessions read-side
    /// (Paso 6.h epic A, hq-u955) and supply the agent event relay (hq-mc72.10). `main.rs`
    /// passes [`SessionsRead::Dolt`] when `GT_DOLT_URL` is set and `Some(root.agent_events)`
    /// so tool-driven session changes reach the log; tests keep the default actor backend
    /// and `None` relay via [`McpService::new`].
    ///
    /// Rig domain (hq-mc72.12.29) wires through [`McpService::with_rig`]; this constructor
    /// keeps the bus's rig slot empty so existing test ctors stay one-call.
    #[allow(clippy::too_many_arguments)]
    pub fn with_sessions(
        agent: AgentHandle,
        sessions: SessionsRead,
        merge: gt_merge::actor::MergeHandle,
        sched: gt_scheduling::actor::SchedHandle,
        patrol: gt_patrol::actor::PatrolHandle,
        orch: gt_orchestration::actor::OrchHandle,
        quota: gt_quota::actor::QuotaHandle,
        scope: Scope,
        audit: Arc<dyn AuditSink>,
        agent_events: Option<mpsc::Sender<Envelope<AgentEvent>>>,
    ) -> Self {
        let bus = CommandBus::new(agent, merge, sched, patrol, orch, quota);
        Self::from_bus(bus, sessions, scope, audit, agent_events)
    }

    /// Builder-style setter for the rig catalog handle (hq-mc72.12.29). Returns a fresh
    /// [`McpService`] whose bus exposes the rig actor; before this is called, `rig.*`
    /// tools return `AppError::Other("rig domain not wired")` and `gt://rigs` returns an
    /// empty array.
    pub fn with_rig(self, rig: RigHandle) -> Self {
        let prev = &*self.inner;
        Self {
            inner: Arc::new(Inner {
                bus: prev.bus.clone().with_rig(rig),
                sessions: prev.sessions.clone(),
                issues: prev.issues.clone(),
                scope: prev.scope.clone(),
                audit: prev.audit.clone(),
                agent_events: prev.agent_events.clone(),
            }),
        }
    }

    /// Scope check + bus dispatch + audit (hq-fe-api-w.1). Every domain-command tool
    /// goes through here — the per-domain `run_*` siblings are thin shims that wrap
    /// their command into the [`RootCommand`] tag. The MCP frontier owns scope, audit
    /// and the edge-only `Spawned` relay; the bus owns the actor-routing.
    pub async fn run_command(
        &self,
        tool: &str,
        arguments: serde_json::Value,
        cmd: RootCommand,
        validate_only: bool,
    ) -> Result<CallToolResult, McpError> {
        if let Err(err) = self.inner.scope.check(tool) {
            self.inner.audit.record(AuditEvent::Unauthorized {
                actor: self.inner.scope.actor.clone(),
                tool: tool.to_string(),
                reason: err.to_string(),
            });
            return Err(McpError::invalid_request(err.to_string(), None));
        }

        // The agent actor mutates its registry but emits nothing. A successful `agent.add`
        // execute must publish `Spawned` on the edge relay so the event reaches the log,
        // the SSE broadcast and the sessions projector — the same path the supervisor/sling
        // edge uses (hq-mc72.10). `new()` matches the polecat default of `Session::new`, so
        // the emitted event and the actor snapshot agree. Pull the event out before
        // moving `cmd` into the dispatcher.
        let spawn_event = match (&cmd, validate_only) {
            (RootCommand::Agent(AgentCommand::Add(a)), false) => Some(AgentEvent::Spawned {
                session: a.id.clone(),
                rig: a.rig.clone(),
                role: SessionRole::Polecat,
                crew: None,
            }),
            _ => None,
        };

        let domain_result = if validate_only {
            self.inner.bus.validate(&cmd, None).await
        } else {
            self.inner.bus.dispatch(cmd, None).await
        };

        let outcome = match &domain_result {
            Ok(()) => Outcome::Ok,
            Err(e) => Outcome::Failed { error: e.to_string() },
        };
        self.inner.audit.record(AuditEvent::Invoked {
            actor: self.inner.scope.actor.clone(),
            tool: tool.to_string(),
            arguments,
            outcome,
        });

        // Best-effort emit: a full mailbox must not fail the tool call. The actor snapshot
        // already reflects the add and the audit record above captured the invocation.
        if domain_result.is_ok() {
            if let (Some(ev), Some(tx)) = (spawn_event, &self.inner.agent_events) {
                let _ = tx.send(Envelope::root(ev)).await;
            }
        }

        match domain_result {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text("ok")])),
            Err(err) => Err(McpError::internal_error(err.to_string(), None)),
        }
    }

    /// Pre-bus legacy entry point (`AgentCommand` only). Kept so existing tests that drive
    /// `service.run("agent.add.execute", ..., AgentCommand::Add(...), ...)` keep compiling.
    /// New code should call [`Self::run_command`] with `RootCommand::Agent(...)`.
    pub async fn run(
        &self,
        tool: &str,
        arguments: serde_json::Value,
        cmd: AgentCommand,
        validate_only: bool,
    ) -> Result<CallToolResult, McpError> {
        self.run_command(tool, arguments, RootCommand::Agent(cmd), validate_only)
            .await
    }

    #[tool(
        name = "agent.add.validate",
        description = "Check whether adding a session would be accepted. No state change."
    )]
    async fn agent_add_validate(
        &self,
        Parameters(args): Parameters<AddSession>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("AddSession is Serialize");
        self.run("agent.add.validate", json, AgentCommand::Add(args), true).await
    }

    #[tool(
        name = "agent.add.execute",
        description = "Add a new session to the registry. Validates first inside the actor."
    )]
    async fn agent_add_execute(
        &self,
        Parameters(args): Parameters<AddSession>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("AddSession is Serialize");
        self.run("agent.add.execute", json, AgentCommand::Add(args), false).await
    }

    #[tool(
        name = "agent.remove.validate",
        description = "Check whether removing a session would be accepted. No state change."
    )]
    async fn agent_remove_validate(
        &self,
        Parameters(args): Parameters<RemoveSession>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("RemoveSession is Serialize");
        self.run("agent.remove.validate", json, AgentCommand::Remove(args), true).await
    }

    #[tool(
        name = "agent.remove.execute",
        description = "Remove a session from the registry."
    )]
    async fn agent_remove_execute(
        &self,
        Parameters(args): Parameters<RemoveSession>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("RemoveSession is Serialize");
        self.run("agent.remove.execute", json, AgentCommand::Remove(args), false).await
    }

    #[tool(
        name = "agent.transition.validate",
        description = "Check whether a lifecycle transition would be accepted. No state change."
    )]
    async fn agent_transition_validate(
        &self,
        Parameters(args): Parameters<TransitionSession>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("TransitionSession is Serialize");
        self.run(
            "agent.transition.validate",
            json,
            AgentCommand::Transition(args),
            true,
        )
        .await
    }

    #[tool(
        name = "agent.transition.execute",
        description = "Transition a session to a new lifecycle state. Illegal transitions are rejected."
    )]
    async fn agent_transition_execute(
        &self,
        Parameters(args): Parameters<TransitionSession>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("TransitionSession is Serialize");
        self.run(
            "agent.transition.execute",
            json,
            AgentCommand::Transition(args),
            false,
        )
        .await
    }

    /// Merge-domain shim around [`Self::run_command`] (hq-fe-api-w.1).
    pub async fn run_merge(
        &self,
        tool: &str,
        arguments: serde_json::Value,
        cmd: MergeCommand,
        validate_only: bool,
    ) -> Result<CallToolResult, McpError> {
        self.run_command(tool, arguments, RootCommand::Merge(cmd), validate_only)
            .await
    }

    #[tool(
        name = "merge.submit.validate",
        description = "Check whether submitting a merge request would be accepted. No state change."
    )]
    async fn merge_submit_validate(
        &self,
        Parameters(args): Parameters<SubmitMerge>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("SubmitMerge is Serialize");
        self.run_merge("merge.submit.validate", json, MergeCommand::Submit(args), true).await
    }

    #[tool(
        name = "merge.submit.execute",
        description = "Register a new merge slot in Ready. Validates first inside the actor."
    )]
    async fn merge_submit_execute(
        &self,
        Parameters(args): Parameters<SubmitMerge>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("SubmitMerge is Serialize");
        self.run_merge("merge.submit.execute", json, MergeCommand::Submit(args), false).await
    }

    // hq-mcyc.1: `merge.start` is reactor-internal — the composition root auto-advances
    // Ready -> Merging when it observes MergeEvent::Ready (see bins/gt::root::handle_event).
    // The previous MCP tools always hit `merging -> merging` and rejected, so they were
    // dead surface area; the StartMerge command itself stays available for the reactor.

    #[tool(
        name = "merge.complete.validate",
        description = "Check whether completing a merge (Merging -> Merged) would be accepted. No state change."
    )]
    async fn merge_complete_validate(
        &self,
        Parameters(args): Parameters<CompleteMerge>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("CompleteMerge is Serialize");
        self.run_merge("merge.complete.validate", json, MergeCommand::Complete(args), true).await
    }

    #[tool(
        name = "merge.complete.execute",
        description = "Mark a merge slot Merging -> Merged with the resulting sha."
    )]
    async fn merge_complete_execute(
        &self,
        Parameters(args): Parameters<CompleteMerge>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("CompleteMerge is Serialize");
        self.run_merge("merge.complete.execute", json, MergeCommand::Complete(args), false).await
    }

    #[tool(
        name = "merge.fail.validate",
        description = "Check whether failing a merge (Merging -> Failed) would be accepted. No state change."
    )]
    async fn merge_fail_validate(
        &self,
        Parameters(args): Parameters<FailMerge>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("FailMerge is Serialize");
        self.run_merge("merge.fail.validate", json, MergeCommand::Fail(args), true).await
    }

    #[tool(
        name = "merge.fail.execute",
        description = "Mark a merge slot Merging -> Failed with a reason."
    )]
    async fn merge_fail_execute(
        &self,
        Parameters(args): Parameters<FailMerge>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("FailMerge is Serialize");
        self.run_merge("merge.fail.execute", json, MergeCommand::Fail(args), false).await
    }

    /// Scheduling-domain shim around [`Self::run_command`] (hq-fe-api-w.1).
    pub async fn run_sched(
        &self,
        tool: &str,
        arguments: serde_json::Value,
        cmd: SchedCommand,
        validate_only: bool,
    ) -> Result<CallToolResult, McpError> {
        self.run_command(tool, arguments, RootCommand::Sched(cmd), validate_only)
            .await
    }

    #[tool(
        name = "scheduling.enqueue.validate",
        description = "Check whether enqueueing a bead would be accepted. No state change."
    )]
    async fn scheduling_enqueue_validate(
        &self,
        Parameters(args): Parameters<Enqueue>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("Enqueue is Serialize");
        self.run_sched("scheduling.enqueue.validate", json, SchedCommand::Enqueue(args), true).await
    }

    #[tool(
        name = "scheduling.enqueue.execute",
        description = "Enqueue a bead at a priority (0=P0..2=P2). The dispatcher pumps it when capacity frees."
    )]
    async fn scheduling_enqueue_execute(
        &self,
        Parameters(args): Parameters<Enqueue>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("Enqueue is Serialize");
        self.run_sched("scheduling.enqueue.execute", json, SchedCommand::Enqueue(args), false).await
    }

    #[tool(
        name = "scheduling.mark_dispatched.validate",
        description = "Check whether manually assigning a bead to a worker would be accepted. No state change."
    )]
    async fn scheduling_mark_dispatched_validate(
        &self,
        Parameters(args): Parameters<MarkDispatched>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("MarkDispatched is Serialize");
        self.run_sched(
            "scheduling.mark_dispatched.validate",
            json,
            SchedCommand::MarkDispatched(args),
            true,
        )
        .await
    }

    #[tool(
        name = "scheduling.mark_dispatched.execute",
        description = "Manually assign a bead to a worker, consuming a capacity slot. Emits scheduling.dispatched."
    )]
    async fn scheduling_mark_dispatched_execute(
        &self,
        Parameters(args): Parameters<MarkDispatched>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("MarkDispatched is Serialize");
        self.run_sched(
            "scheduling.mark_dispatched.execute",
            json,
            SchedCommand::MarkDispatched(args),
            false,
        )
        .await
    }

    /// Scope check + (validate | upsert) + audit for `scheduling.create_bead`. Bead creation
    /// is a repo write, not a queue command, so it bypasses the `SchedCommand` path and emits
    /// no `SchedEvent`; only the frontier audit records the invocation (hq-mc72.10).
    pub async fn run_create_bead(
        &self,
        tool: &str,
        args: CreateBead,
        validate_only: bool,
    ) -> Result<CallToolResult, McpError> {
        if let Err(err) = self.inner.scope.check(tool) {
            self.inner.audit.record(AuditEvent::Unauthorized {
                actor: self.inner.scope.actor.clone(),
                tool: tool.to_string(),
                reason: err.to_string(),
            });
            return Err(McpError::invalid_request(err.to_string(), None));
        }

        let json = serde_json::to_value(&args).expect("CreateBead is Serialize");
        let domain_result = if validate_only {
            args.validate()
        } else {
            match args.validate() {
                Ok(()) => self.inner.bus.sched().create_bead(args.to_bead()).await,
                Err(e) => Err(e),
            }
        };

        let outcome = match &domain_result {
            Ok(()) => Outcome::Ok,
            Err(e) => Outcome::Failed { error: e.to_string() },
        };
        self.inner.audit.record(AuditEvent::Invoked {
            actor: self.inner.scope.actor.clone(),
            tool: tool.to_string(),
            arguments: json,
            outcome,
        });

        match domain_result {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text("ok")])),
            Err(err) => Err(McpError::internal_error(err.to_string(), None)),
        }
    }

    #[tool(
        name = "scheduling.create_bead.validate",
        description = "Check whether creating a bead would be accepted. No state change."
    )]
    async fn scheduling_create_bead_validate(
        &self,
        Parameters(args): Parameters<CreateBead>,
    ) -> Result<CallToolResult, McpError> {
        self.run_create_bead("scheduling.create_bead.validate", args, true).await
    }

    #[tool(
        name = "scheduling.create_bead.execute",
        description = "Create a pending bead in the repo so the dispatcher can claim it. Not event-logged."
    )]
    async fn scheduling_create_bead_execute(
        &self,
        Parameters(args): Parameters<CreateBead>,
    ) -> Result<CallToolResult, McpError> {
        self.run_create_bead("scheduling.create_bead.execute", args, false).await
    }

    /// Patrol-domain shim around [`Self::run_command`] (hq-fe-api-w.1). The lease tracker
    /// may emit several `LeaseExpired`s for a single `Tick`; the actor relays them — the
    /// shim only reports the single ok/err the dispatch returned.
    pub async fn run_patrol(
        &self,
        tool: &str,
        arguments: serde_json::Value,
        cmd: PatrolCommand,
        validate_only: bool,
    ) -> Result<CallToolResult, McpError> {
        self.run_command(tool, arguments, RootCommand::Patrol(cmd), validate_only)
            .await
    }

    #[tool(
        name = "patrol.register.validate",
        description = "Check whether opening a lease would be accepted. No state change."
    )]
    async fn patrol_register_validate(
        &self,
        Parameters(args): Parameters<RegisterLease>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("RegisterLease is Serialize");
        self.run_patrol("patrol.register.validate", json, PatrolCommand::Register(args), true).await
    }

    #[tool(
        name = "patrol.register.execute",
        description = "Open a lease for a dispatched bead/worker. Emits patrol.lease_registered."
    )]
    async fn patrol_register_execute(
        &self,
        Parameters(args): Parameters<RegisterLease>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("RegisterLease is Serialize");
        self.run_patrol("patrol.register.execute", json, PatrolCommand::Register(args), false).await
    }

    #[tool(
        name = "patrol.heartbeat.validate",
        description = "Check whether a worker heartbeat would be accepted. No state change."
    )]
    async fn patrol_heartbeat_validate(
        &self,
        Parameters(args): Parameters<Heartbeat>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("Heartbeat is Serialize");
        self.run_patrol("patrol.heartbeat.validate", json, PatrolCommand::Heartbeat(args), true).await
    }

    #[tool(
        name = "patrol.heartbeat.execute",
        description = "Record a worker heartbeat, refreshing every lease it owns. Emits patrol.heartbeat."
    )]
    async fn patrol_heartbeat_execute(
        &self,
        Parameters(args): Parameters<Heartbeat>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("Heartbeat is Serialize");
        self.run_patrol("patrol.heartbeat.execute", json, PatrolCommand::Heartbeat(args), false).await
    }

    #[tool(
        name = "patrol.close.validate",
        description = "Check whether closing a lease would be accepted. No state change."
    )]
    async fn patrol_close_validate(
        &self,
        Parameters(args): Parameters<CloseLease>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("CloseLease is Serialize");
        self.run_patrol("patrol.close.validate", json, PatrolCommand::Close(args), true).await
    }

    #[tool(
        name = "patrol.close.execute",
        description = "Close a lease on completion/failure (no expiry fires). Emits patrol.lease_closed."
    )]
    async fn patrol_close_execute(
        &self,
        Parameters(args): Parameters<CloseLease>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("CloseLease is Serialize");
        self.run_patrol("patrol.close.execute", json, PatrolCommand::Close(args), false).await
    }

    #[tool(
        name = "patrol.tick.validate",
        description = "Check whether running the stale-lease detector would be accepted. No state change."
    )]
    async fn patrol_tick_validate(
        &self,
        Parameters(args): Parameters<Tick>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("Tick is Serialize");
        self.run_patrol("patrol.tick.validate", json, PatrolCommand::Tick(args), true).await
    }

    #[tool(
        name = "patrol.tick.execute",
        description = "Run the stale-lease detector at now_secs; emits patrol.lease_expired for each lease past timeout."
    )]
    async fn patrol_tick_execute(
        &self,
        Parameters(args): Parameters<Tick>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("Tick is Serialize");
        self.run_patrol("patrol.tick.execute", json, PatrolCommand::Tick(args), false).await
    }

    /// Orchestration-domain shim around [`Self::run_command`] (hq-fe-api-w.1).
    pub async fn run_orch(
        &self,
        tool: &str,
        arguments: serde_json::Value,
        cmd: OrchCommand,
        validate_only: bool,
    ) -> Result<CallToolResult, McpError> {
        self.run_command(tool, arguments, RootCommand::Orch(cmd), validate_only)
            .await
    }

    #[tool(
        name = "orch.launch_convoy.validate",
        description = "Check whether creating + launching a convoy would be accepted. No state change."
    )]
    async fn orch_launch_convoy_validate(
        &self,
        Parameters(args): Parameters<LaunchConvoy>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("LaunchConvoy is Serialize");
        self.run_orch("orch.launch_convoy.validate", json, OrchCommand::Launch(args), true).await
    }

    #[tool(
        name = "orch.launch_convoy.execute",
        description = "Create + launch a convoy and dispatch its first member. Emits convoy_created/launched/member_dispatched."
    )]
    async fn orch_launch_convoy_execute(
        &self,
        Parameters(args): Parameters<LaunchConvoy>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("LaunchConvoy is Serialize");
        self.run_orch("orch.launch_convoy.execute", json, OrchCommand::Launch(args), false).await
    }

    #[tool(
        name = "orch.complete_member.validate",
        description = "Check whether completing a convoy member would be accepted. No state change."
    )]
    async fn orch_complete_member_validate(
        &self,
        Parameters(args): Parameters<CompleteMember>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("CompleteMember is Serialize");
        self.run_orch("orch.complete_member.validate", json, OrchCommand::Complete(args), true).await
    }

    #[tool(
        name = "orch.complete_member.execute",
        description = "Mark a convoy member done; hands off the next member or closes the convoy."
    )]
    async fn orch_complete_member_execute(
        &self,
        Parameters(args): Parameters<CompleteMember>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("CompleteMember is Serialize");
        self.run_orch("orch.complete_member.execute", json, OrchCommand::Complete(args), false).await
    }

    #[tool(
        name = "orch.fail_member.validate",
        description = "Check whether failing a convoy member would be accepted. No state change."
    )]
    async fn orch_fail_member_validate(
        &self,
        Parameters(args): Parameters<FailMember>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("FailMember is Serialize");
        self.run_orch("orch.fail_member.validate", json, OrchCommand::Fail(args), true).await
    }

    #[tool(
        name = "orch.fail_member.execute",
        description = "Mark a convoy member failed; halts the convoy (emits convoy_failed)."
    )]
    async fn orch_fail_member_execute(
        &self,
        Parameters(args): Parameters<FailMember>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("FailMember is Serialize");
        self.run_orch("orch.fail_member.execute", json, OrchCommand::Fail(args), false).await
    }

    /// Quota-domain shim around [`Self::run_command`] (hq-fe-api-w.1).
    pub async fn run_quota(
        &self,
        tool: &str,
        arguments: serde_json::Value,
        cmd: QuotaCommand,
        validate_only: bool,
    ) -> Result<CallToolResult, McpError> {
        self.run_command(tool, arguments, RootCommand::Quota(cmd), validate_only)
            .await
    }

    #[tool(
        name = "quota.sample.validate",
        description = "Check whether recording a token usage sample would be accepted. No state change."
    )]
    async fn quota_sample_validate(
        &self,
        Parameters(args): Parameters<SampleTokens>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("SampleTokens is Serialize");
        self.run_quota("quota.sample.validate", json, QuotaCommand::Sample(args), true).await
    }

    #[tool(
        name = "quota.sample.execute",
        description = "Record a per-session token usage sample; feeds consumption + rate EWMA. Emits quota.tokens_sampled."
    )]
    async fn quota_sample_execute(
        &self,
        Parameters(args): Parameters<SampleTokens>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("SampleTokens is Serialize");
        self.run_quota("quota.sample.execute", json, QuotaCommand::Sample(args), false).await
    }

    #[tool(
        name = "quota.probe.validate",
        description = "Check whether reconciling against provider rate-limit headers would be accepted. No state change."
    )]
    async fn quota_probe_validate(
        &self,
        Parameters(args): Parameters<ProbeWindow>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("ProbeWindow is Serialize");
        self.run_quota("quota.probe.validate", json, QuotaCommand::Probe(args), true).await
    }

    #[tool(
        name = "quota.probe.execute",
        description = "Reconcile the live window against provider remaining/resets. Emits quota.usage_probed."
    )]
    async fn quota_probe_execute(
        &self,
        Parameters(args): Parameters<ProbeWindow>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("ProbeWindow is Serialize");
        self.run_quota("quota.probe.execute", json, QuotaCommand::Probe(args), false).await
    }

    #[tool(
        name = "quota.rotate.validate",
        description = "Check whether rotating off an account would be accepted. No state change."
    )]
    async fn quota_rotate_validate(
        &self,
        Parameters(args): Parameters<RotateAccount>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("RotateAccount is Serialize");
        self.run_quota("quota.rotate.validate", json, QuotaCommand::Rotate(args), true).await
    }

    #[tool(
        name = "quota.rotate.execute",
        description = "Rotate off an account onto a healthy one; parks the source in cooldown. Emits quota.rotated."
    )]
    async fn quota_rotate_execute(
        &self,
        Parameters(args): Parameters<RotateAccount>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("RotateAccount is Serialize");
        self.run_quota("quota.rotate.execute", json, QuotaCommand::Rotate(args), false).await
    }

    /// Scope check + (validate | upsert) + audit for `quota.register`. Mirrors the other
    /// `run_*` helpers but bypasses the `QuotaCommand` path: registration emits no domain
    /// event (see [`RegisterAccount`]), so only the frontier audit records the invocation.
    pub async fn run_quota_register(
        &self,
        tool: &str,
        args: RegisterAccount,
        validate_only: bool,
    ) -> Result<CallToolResult, McpError> {
        if let Err(err) = self.inner.scope.check(tool) {
            self.inner.audit.record(AuditEvent::Unauthorized {
                actor: self.inner.scope.actor.clone(),
                tool: tool.to_string(),
                reason: err.to_string(),
            });
            return Err(McpError::invalid_request(err.to_string(), None));
        }

        let json = serde_json::to_value(&args).expect("RegisterAccount is Serialize");
        let domain_result = args.validate();
        if domain_result.is_ok() && !validate_only {
            self.inner.bus.quota().upsert_account(args.to_account()).await;
        }

        let outcome = match &domain_result {
            Ok(()) => Outcome::Ok,
            Err(e) => Outcome::Failed { error: e.to_string() },
        };
        self.inner.audit.record(AuditEvent::Invoked {
            actor: self.inner.scope.actor.clone(),
            tool: tool.to_string(),
            arguments: json,
            outcome,
        });

        match domain_result {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text("ok")])),
            Err(err) => Err(McpError::internal_error(err.to_string(), None)),
        }
    }

    #[tool(
        name = "quota.register.validate",
        description = "Check whether registering a quota account would be accepted. No state change."
    )]
    async fn quota_register_validate(
        &self,
        Parameters(args): Parameters<RegisterAccount>,
    ) -> Result<CallToolResult, McpError> {
        self.run_quota_register("quota.register.validate", args, true).await
    }

    #[tool(
        name = "quota.register.execute",
        description = "Register (or replace) a quota account with a live window so sample/probe/rotate can act on it. Not event-logged."
    )]
    async fn quota_register_execute(
        &self,
        Parameters(args): Parameters<RegisterAccount>,
    ) -> Result<CallToolResult, McpError> {
        self.run_quota_register("quota.register.execute", args, false).await
    }

    /// Scope check + (validate | remove) + audit for `quota.retire` (hq-mc72.12.25). Symmetric
    /// to [`McpService::run_quota_register`]: drops an account from the registry. Returns
    /// `removed=true` only when an account with the id existed; an id that was not present is
    /// treated as success (`removed=false`) so retire is idempotent.
    pub async fn run_quota_retire(
        &self,
        tool: &str,
        args: RetireAccount,
        validate_only: bool,
    ) -> Result<CallToolResult, McpError> {
        if let Err(err) = self.inner.scope.check(tool) {
            self.inner.audit.record(AuditEvent::Unauthorized {
                actor: self.inner.scope.actor.clone(),
                tool: tool.to_string(),
                reason: err.to_string(),
            });
            return Err(McpError::invalid_request(err.to_string(), None));
        }

        let json = serde_json::to_value(&args).expect("RetireAccount is Serialize");
        let validate_result = args.validate();
        let removed = if validate_result.is_ok() && !validate_only {
            self.inner.bus.quota().remove_account(args.account.clone()).await
        } else {
            false
        };

        let outcome = match &validate_result {
            Ok(()) => Outcome::Ok,
            Err(e) => Outcome::Failed { error: e.to_string() },
        };
        self.inner.audit.record(AuditEvent::Invoked {
            actor: self.inner.scope.actor.clone(),
            tool: tool.to_string(),
            arguments: json,
            outcome,
        });

        match validate_result {
            Ok(()) => {
                let text = if validate_only {
                    "ok".to_string()
                } else {
                    format!("removed={removed}")
                };
                Ok(CallToolResult::success(vec![Content::text(text)]))
            }
            Err(err) => Err(McpError::internal_error(err.to_string(), None)),
        }
    }

    #[tool(
        name = "quota.retire.validate",
        description = "Check whether retiring a quota account would be accepted (non-empty id). No state change."
    )]
    async fn quota_retire_validate(
        &self,
        Parameters(args): Parameters<RetireAccount>,
    ) -> Result<CallToolResult, McpError> {
        self.run_quota_retire("quota.retire.validate", args, true).await
    }

    #[tool(
        name = "quota.retire.execute",
        description = "Drop an account from the quota registry. Returns `removed=true` when the id existed, `removed=false` otherwise (idempotent). Not event-logged."
    )]
    async fn quota_retire_execute(
        &self,
        Parameters(args): Parameters<RetireAccount>,
    ) -> Result<CallToolResult, McpError> {
        self.run_quota_retire("quota.retire.execute", args, false).await
    }

    /// Rig-domain shim around [`Self::run_command`] (hq-fe-api-w.1). The bus surfaces
    /// the same `rig domain not wired` error when [`Self::with_rig`] has not been called,
    /// preserving the pre-bus contract.
    pub async fn run_rig(
        &self,
        tool: &str,
        arguments: serde_json::Value,
        cmd: RigCommand,
        validate_only: bool,
    ) -> Result<CallToolResult, McpError> {
        self.run_command(tool, arguments, RootCommand::Rig(cmd), validate_only)
            .await
    }

    #[tool(
        name = "rig.add.validate",
        description = "Check whether registering a new rig would be accepted (name/prefix grammar, no name/prefix collision). No state change."
    )]
    async fn rig_add_validate(
        &self,
        Parameters(args): Parameters<AddRig>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("AddRig is Serialize");
        self.run_rig("rig.add.validate", json, RigCommand::Add(args), true).await
    }

    #[tool(
        name = "rig.add.execute",
        description = "Register a new rig in the catalog (orchestrator state only; the on-disk clone is a deploy-edge step). Emits rig.added."
    )]
    async fn rig_add_execute(
        &self,
        Parameters(args): Parameters<AddRig>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("AddRig is Serialize");
        self.run_rig("rig.add.execute", json, RigCommand::Add(args), false).await
    }

    #[tool(
        name = "rig.adopt.validate",
        description = "Check whether adopting an existing on-disk rig directory would be accepted. Same validation as rig.add. No state change."
    )]
    async fn rig_adopt_validate(
        &self,
        Parameters(args): Parameters<AdoptRig>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("AdoptRig is Serialize");
        self.run_rig("rig.adopt.validate", json, RigCommand::Adopt(args), true).await
    }

    #[tool(
        name = "rig.adopt.execute",
        description = "Adopt an existing on-disk rig into the catalog without re-cloning. Emits rig.adopted."
    )]
    async fn rig_adopt_execute(
        &self,
        Parameters(args): Parameters<AdoptRig>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("AdoptRig is Serialize");
        self.run_rig("rig.adopt.execute", json, RigCommand::Adopt(args), false).await
    }

    #[tool(
        name = "rig.remove.validate",
        description = "Check whether removing a rig from the catalog would be accepted (must exist). No state change."
    )]
    async fn rig_remove_validate(
        &self,
        Parameters(args): Parameters<RemoveRig>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("RemoveRig is Serialize");
        self.run_rig("rig.remove.validate", json, RigCommand::Remove(args), true).await
    }

    #[tool(
        name = "rig.remove.execute",
        description = "Drop a rig from the catalog (orchestrator loses routing authority; on-disk teardown is a deploy-edge step). Emits rig.removed."
    )]
    async fn rig_remove_execute(
        &self,
        Parameters(args): Parameters<RemoveRig>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("RemoveRig is Serialize");
        self.run_rig("rig.remove.execute", json, RigCommand::Remove(args), false).await
    }

    #[tool(
        name = "rig.set_prefix.validate",
        description = "Check whether changing a rig's beads prefix would be accepted (grammar, no collision, not a no-op). No state change."
    )]
    async fn rig_set_prefix_validate(
        &self,
        Parameters(args): Parameters<SetRigPrefix>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("SetRigPrefix is Serialize");
        self.run_rig("rig.set_prefix.validate", json, RigCommand::SetPrefix(args), true).await
    }

    #[tool(
        name = "rig.set_prefix.execute",
        description = "Change a rig's beads prefix (the matching bd config set issue_prefix is a deploy-edge side-effect). Emits rig.prefix_changed."
    )]
    async fn rig_set_prefix_execute(
        &self,
        Parameters(args): Parameters<SetRigPrefix>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("SetRigPrefix is Serialize");
        self.run_rig("rig.set_prefix.execute", json, RigCommand::SetPrefix(args), false).await
    }

    #[tool(
        name = "rig.set_default_branch.validate",
        description = "Check whether changing a rig's default branch would be accepted (non-empty, not a no-op). No state change."
    )]
    async fn rig_set_default_branch_validate(
        &self,
        Parameters(args): Parameters<SetRigDefaultBranch>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("SetRigDefaultBranch is Serialize");
        self.run_rig(
            "rig.set_default_branch.validate",
            json,
            RigCommand::SetDefaultBranch(args),
            true,
        )
        .await
    }

    #[tool(
        name = "rig.set_default_branch.execute",
        description = "Change the default branch tracked for a rig. Emits rig.default_branch_changed."
    )]
    async fn rig_set_default_branch_execute(
        &self,
        Parameters(args): Parameters<SetRigDefaultBranch>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("SetRigDefaultBranch is Serialize");
        self.run_rig(
            "rig.set_default_branch.execute",
            json,
            RigCommand::SetDefaultBranch(args),
            false,
        )
        .await
    }

    // --- meta: server self-description (hq-mcp-onboard.7) -----------------------------------

    #[tool(
        name = "meta.help",
        description = "Server self-description: gt-mcp version, full tool index (names + descriptions), and resource catalog (URIs + descriptions). Single-call discovery — substitutes tools/list + resources/list. No state change, no actor dispatch."
    )]
    async fn meta_help(&self) -> Result<CallToolResult, McpError> {
        let tool = "meta.help";
        if let Err(err) = self.inner.scope.check(tool) {
            self.inner.audit.record(AuditEvent::Unauthorized {
                actor: self.inner.scope.actor.clone(),
                tool: tool.to_string(),
                reason: err.to_string(),
            });
            return Err(McpError::invalid_request(err.to_string(), None));
        }

        let payload = self.meta_help_payload();
        self.inner.audit.record(AuditEvent::Invoked {
            actor: self.inner.scope.actor.clone(),
            tool: tool.to_string(),
            arguments: serde_json::Value::Null,
            outcome: Outcome::Ok,
        });

        let text = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string());
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(
        name = "meta.report_gap",
        description = "Surface a missing MCP operation: server mints a `hq-gap-<slug>-<ts>` bead so the gap enters the routine catalog. Closes the loop the gap-discipline doc (§4) prescribes. Input: { operation (required, canonical name like `issues.update.execute`), notes (optional context), priority (optional u8, 0=P0..2=P2, defaults to P2) }."
    )]
    async fn meta_report_gap(
        &self,
        Parameters(args): Parameters<ReportGap>,
    ) -> Result<CallToolResult, McpError> {
        self.run_report_gap(args).await
    }

    /// Scope check + validate + mint the gap bead + audit. Plain method so tests drive
    /// the same code path the tool dispatch hits, no `RequestContext` needed (mirrors
    /// `run_create_bead`).
    pub async fn run_report_gap(
        &self,
        args: ReportGap,
    ) -> Result<CallToolResult, McpError> {
        let tool = "meta.report_gap";
        if let Err(err) = self.inner.scope.check(tool) {
            self.inner.audit.record(AuditEvent::Unauthorized {
                actor: self.inner.scope.actor.clone(),
                tool: tool.to_string(),
                reason: err.to_string(),
            });
            return Err(McpError::invalid_request(err.to_string(), None));
        }

        let json = serde_json::to_value(&args).expect("ReportGap is Serialize");
        let domain_result = match args.validate() {
            Ok(()) => {
                let create = args.to_create_bead();
                let bead = create.to_bead();
                let bead_id = create.id.clone();
                match self.inner.bus.sched().create_bead(bead).await {
                    Ok(()) => Ok(bead_id),
                    Err(e) => Err(e),
                }
            }
            Err(e) => Err(e),
        };

        let outcome = match &domain_result {
            Ok(_) => Outcome::Ok,
            Err(e) => Outcome::Failed { error: e.to_string() },
        };
        self.inner.audit.record(AuditEvent::Invoked {
            actor: self.inner.scope.actor.clone(),
            tool: tool.to_string(),
            arguments: json,
            outcome,
        });

        match domain_result {
            Ok(bead_id) => {
                let body = serde_json::json!({
                    "bead": bead_id,
                    "operation": args.operation,
                    "priority": args.effective_priority(),
                });
                let text = serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string());
                Ok(CallToolResult::success(vec![Content::text(text)]))
            }
            Err(err) => Err(McpError::internal_error(err.to_string(), None)),
        }
    }

    /// Build the JSON payload returned by `meta.help`. Kept as a plain method so tests can
    /// drive it without the scope + audit layer — same shape the wire serves.
    pub fn meta_help_payload(&self) -> serde_json::Value {
        let tools: Vec<serde_json::Value> = Self::tool_router()
            .list_all()
            .into_iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                })
            })
            .collect();
        let resources: Vec<serde_json::Value> = self
            .resource_list()
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "uri": r.uri,
                    "name": r.name,
                    "description": r.description,
                })
            })
            .collect();
        serde_json::json!({
            "server": {
                "name": env!("CARGO_PKG_NAME"),
                "version": env!("CARGO_PKG_VERSION"),
            },
            "tools": tools,
            "resources": resources,
        })
    }

    // --- read-side: domain snapshots exposed as MCP Resources (doc 09 row 1) ----------------
    //
    // These are the read queries the rmcp ServerHandler exposes via `list_resources` /
    // `read_resource` below. Kept as plain methods so tests drive them without standing up a
    // RequestContext — same split as the tool `run_*` helpers.

    /// The catalog of read-only snapshot resources, one per domain. Each reads JSON via
    /// [`McpService::read_resource_json`] at the matching `uri`.
    pub fn resource_list(&self) -> Vec<RawResource> {
        let mk = |uri: &str, name: &str, desc: &str| {
            let mut r = RawResource::new(uri, name);
            r.description = Some(desc.to_string());
            r.mime_type = Some("application/json".to_string());
            r
        };
        vec![
            mk("gt://agent/sessions", "agent.sessions", "Active agent sessions and their lifecycle state."),
            mk("gt://scheduling/queue", "scheduling.queue", "Dispatcher queue depth and in-flight capacity."),
            mk("gt://patrol/leases", "patrol.leases", "Live lease count and total expirations emitted."),
            mk("gt://merge/slots", "merge.slots", "Merge slots and their state machine position."),
            mk("gt://orch/convoys", "orch.convoys", "Convoys, their state and per-member progress."),
            mk("gt://quota/accounts", "quota.accounts", "Tracked account count and predictions emitted."),
            mk("gt://rigs", "rigs", "Registered rigs in the catalog (name, prefix, git remotes, default branch)."),
            mk(
                "gt://issues",
                "issues",
                "Canonical issues snapshot from Dolt. Filters via querystring: status=open[,working], priority_max=2, assignee=X, external_ref=Y, issue_type=epic, limit=N.",
            ),
        ]
    }

    /// Read one snapshot resource as JSON. Unknown uri → `NotFound`. Pure read: it only
    /// snapshots the shared actors, never mutates.
    pub async fn read_resource_json(&self, uri: &str) -> Result<serde_json::Value, AppError> {
        // `gt://issues` is the only resource that consumes a querystring today
        // (hq-mcp-issues.1). Split once on '?' so the other resource arms keep
        // matching the exact-string URIs they already handle. The bare path and
        // the `?...`-prefixed form both route here; anything else (e.g.
        // `gt://issuesX`) falls through to the `NotFound` arm below.
        let issues_match = match uri.strip_prefix("gt://issues") {
            Some("") => Some(""),
            Some(rest) if rest.starts_with('?') => Some(&rest[1..]),
            _ => None,
        };
        if let Some(qs) = issues_match {
            let filter = parse_issue_filter(qs)?;
            return self.inner.issues.snapshot(&filter).await;
        }
        match uri {
            "gt://agent/sessions" => {
                let sessions = self.inner.sessions.snapshot().await;
                serde_json::to_value(&sessions)
                    .map_err(|e| AppError::Other(format!("encode sessions: {e}")))
            }
            "gt://scheduling/queue" => {
                let (queued, in_flight) = self.inner.bus.sched().snapshot().await;
                Ok(serde_json::json!({ "queued": queued, "in_flight": in_flight }))
            }
            "gt://patrol/leases" => {
                let (live_leases, expired_emitted) = self.inner.bus.patrol().snapshot().await;
                Ok(serde_json::json!({ "live_leases": live_leases, "expired_emitted": expired_emitted }))
            }
            "gt://merge/slots" => {
                let slots = self.inner.bus.merge().snapshot().await;
                let arr: Vec<serde_json::Value> = slots
                    .iter()
                    .map(|s| serde_json::json!({ "bead": s.bead, "branch": s.branch, "state": s.state.as_str() }))
                    .collect();
                Ok(serde_json::Value::Array(arr))
            }
            "gt://orch/convoys" => {
                let convoys = self.inner.bus.orch().snapshot().await;
                let arr: Vec<serde_json::Value> = convoys
                    .iter()
                    .map(|c| {
                        let members: Vec<serde_json::Value> = c
                            .members
                            .iter()
                            .map(|m| serde_json::json!({ "bead": m.bead, "state": m.state.as_str() }))
                            .collect();
                        serde_json::json!({ "id": c.id, "state": c.state.as_str(), "members": members })
                    })
                    .collect();
                Ok(serde_json::Value::Array(arr))
            }
            "gt://quota/accounts" => {
                let (accounts, predictions_emitted) = self.inner.bus.quota().snapshot().await;
                Ok(serde_json::json!({ "accounts": accounts, "predictions_emitted": predictions_emitted }))
            }
            "gt://rigs" => {
                // Empty array until the composition root wires the actor (TODO hq-mc72.12.30).
                // RigEntry is Serialize, so once `rig` is Some this is the catalog snapshot.
                let rigs = match self.inner.bus.rig() {
                    Some(rig) => rig.rigs().await,
                    None => Vec::new(),
                };
                serde_json::to_value(&rigs)
                    .map_err(|e| AppError::Other(format!("encode rigs: {e}")))
            }
            other => Err(AppError::NotFound(format!("resource {other}"))),
        }
    }
}

/// Parse the optional `gt://issues?...` querystring into an [`IssueFilter`].
/// Empty input → default filter (no `WHERE` clauses). Percent-decoding is not
/// applied because every supported field carries ASCII tokens (status names,
/// bead ids, ints); unknown keys are returned as `Validation` so the operator
/// learns about typos instead of silently getting the unfiltered set.
fn parse_issue_filter(qs: &str) -> Result<IssueFilter, AppError> {
    let mut filter = IssueFilter::default();
    if qs.is_empty() {
        return Ok(filter);
    }
    for pair in qs.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = match pair.split_once('=') {
            Some(kv) => kv,
            None => return Err(AppError::Validation(format!("missing `=` in `{pair}`"))),
        };
        match key {
            "status" => {
                filter.status = value
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .collect();
            }
            "priority_max" => {
                filter.priority_max = Some(
                    value
                        .parse::<u8>()
                        .map_err(|e| AppError::Validation(format!("priority_max: {e}")))?,
                );
            }
            "assignee" => filter.assignee = Some(value.to_string()),
            "external_ref" => filter.external_ref = Some(value.to_string()),
            "issue_type" => filter.issue_type = Some(value.to_string()),
            "limit" => {
                filter.limit = Some(
                    value
                        .parse::<u32>()
                        .map_err(|e| AppError::Validation(format!("limit: {e}")))?,
                );
            }
            other => return Err(AppError::Validation(format!("unknown filter `{other}`"))),
        }
    }
    Ok(filter)
}

/// The rmcp server handler. `#[tool_handler]` injects `call_tool`/`list_tools`/`get_tool`
/// from the `#[tool_router]` registry; we add the read-side (`list_resources`/`read_resource`)
/// and a `get_info` that advertises both the tools and resources capabilities.
#[tool_handler(router = Self::tool_router())]
impl ServerHandler for McpService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::from_build_env())
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let resources = self.resource_list().into_iter().map(|r| r.no_annotation()).collect();
        Ok(ListResourcesResult::with_all_items(resources))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        match self.read_resource_json(&request.uri).await {
            Ok(value) => {
                let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
                Ok(ReadResourceResult::new(vec![ResourceContents::text(text, &request.uri)]))
            }
            Err(e) => Err(McpError::invalid_request(e.to_string(), None)),
        }
    }
}
