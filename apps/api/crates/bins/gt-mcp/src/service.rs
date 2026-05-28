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

use std::path::PathBuf;
use std::sync::Arc;

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

use gt_agent::actor::AgentHandle;
use gt_agent::{
    AddSession, AgentCommand, AgentEvent, RemoveSession, Session, SessionQueries, SessionRole,
    TransitionSession,
};
use gt_beads::{Bead, BeadStatus};
use gt_store_dolt::DoltSessions;
use gt_merge::actor::MergeHandle;
use gt_merge::{CompleteMerge, FailMerge, MergeCommand, StartMerge, SubmitMerge};
use gt_orchestration::actor::OrchHandle;
use gt_orchestration::{CompleteMember, FailMember, LaunchConvoy, OrchCommand};
use gt_patrol::actor::PatrolHandle;
use gt_patrol::{CloseLease, Heartbeat, PatrolCommand, RegisterLease, Tick};
use gt_scheduling::actor::SchedHandle;
use gt_scheduling::{Enqueue, MarkDispatched, SchedCommand};
use gt_quota::actor::QuotaHandle;
use gt_quota::{
    Account, AccountQuotaStatus, AccountWindow, ProbeWindow, QuotaCommand, RotateAccount,
    SampleTokens, WindowKind,
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

/// Edge that creates rigs by shelling out to the `gt` CLI (hq-mc72.11). gt-mcp has no rig
/// domain — rigs are a Gas Town CLI concept (`gt rig add`), not orchestrator state — so this
/// is a bin-level effect, the same shape as `RealEffects::sling` running `gt sling`. Args are
/// passed directly to `Command` (never a shell), so a crafted `name`/`git_url` cannot inject.
#[derive(Clone)]
pub struct RigCreator {
    /// Path to the `gt` binary (or just `gt` to resolve on PATH).
    pub gt_bin: PathBuf,
}

impl RigCreator {
    /// Run `gt rig add <name> <git_url> [--prefix <prefix>]`, returning stdout on success or
    /// the captured stderr on failure. Blocks until `gt rig add` finishes (it clones a repo).
    async fn create(&self, args: &CreateRig) -> Result<String, AppError> {
        let mut cmd = tokio::process::Command::new(&self.gt_bin);
        cmd.arg("rig").arg("add").arg(&args.name).arg(&args.git_url);
        if let Some(prefix) = &args.prefix {
            cmd.arg("--prefix").arg(prefix);
        }
        let out = cmd
            .output()
            .await
            .map_err(|e| AppError::Other(format!("spawn `{} rig add`: {e}", self.gt_bin.display())))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(AppError::Other(format!(
                "gt rig add failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )))
        }
    }
}

/// Input for the `rig.create` tool (hq-mc72.11). Creates a Gas Town rig via `gt rig add`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct CreateRig {
    /// Rig name. Restricted to `[A-Za-z0-9_-]` (it becomes a directory + CLI argument).
    pub name: String,
    /// Git URL (or local `file://` path) to clone as the rig's canonical repo.
    pub git_url: String,
    /// Beads issue prefix. Defaults to one derived from the name when omitted.
    #[serde(default)]
    pub prefix: Option<String>,
}

impl CreateRig {
    fn validate(&self) -> Result<(), AppError> {
        if self.name.is_empty() {
            return Err(AppError::Validation("rig name is empty".into()));
        }
        if !self
            .name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(AppError::Validation(
                "rig name must be [A-Za-z0-9_-]".into(),
            ));
        }
        if self.git_url.is_empty() {
            return Err(AppError::Validation("git_url is empty".into()));
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct McpService {
    inner: Arc<Inner>,
}

struct Inner {
    agent: AgentHandle,
    sessions: SessionsRead,
    merge: MergeHandle,
    sched: SchedHandle,
    patrol: PatrolHandle,
    orch: OrchHandle,
    quota: QuotaHandle,
    scope: Scope,
    audit: Arc<dyn AuditSink>,
    /// Edge relay for agent events. The agent actor is relay-less by design (the supervisor
    /// and spawn edges emit on it — see `bins/gt::root`), so a tool-driven `agent.add` must
    /// publish `Spawned` here itself for the event to reach the log/broadcast/projector.
    /// `None` in tests built via [`McpService::new`]; `main.rs` wires the root's relay.
    agent_events: Option<mpsc::Sender<Envelope<AgentEvent>>>,
    /// Edge that creates rigs via `gt rig add` (hq-mc72.11). `None` in tests (the `rig.create`
    /// tool then reports "rig creation not configured"); `main.rs` wires it from `GT_BIN`.
    rig_creator: Option<RigCreator>,
}

#[tool_router]
impl McpService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        agent: AgentHandle,
        merge: MergeHandle,
        sched: SchedHandle,
        patrol: PatrolHandle,
        orch: OrchHandle,
        quota: QuotaHandle,
        scope: Scope,
        audit: Arc<dyn AuditSink>,
    ) -> Self {
        let sessions = SessionsRead::Actor(agent.clone());
        Self::with_sessions(
            agent, sessions, merge, sched, patrol, orch, quota, scope, audit, None, None,
        )
    }

    /// Same as [`McpService::new`] but lets the caller override the sessions read-side
    /// (Paso 6.h epic A, hq-u955) and supply the agent event relay (hq-mc72.10). `main.rs`
    /// passes [`SessionsRead::Dolt`] when `GT_DOLT_URL` is set and `Some(root.agent_events)`
    /// so tool-driven session changes reach the log; tests keep the default actor backend
    /// and `None` relay via [`McpService::new`].
    #[allow(clippy::too_many_arguments)]
    pub fn with_sessions(
        agent: AgentHandle,
        sessions: SessionsRead,
        merge: MergeHandle,
        sched: SchedHandle,
        patrol: PatrolHandle,
        orch: OrchHandle,
        quota: QuotaHandle,
        scope: Scope,
        audit: Arc<dyn AuditSink>,
        agent_events: Option<mpsc::Sender<Envelope<AgentEvent>>>,
        rig_creator: Option<RigCreator>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                agent,
                sessions,
                merge,
                sched,
                patrol,
                orch,
                quota,
                scope,
                audit,
                agent_events,
                rig_creator,
            }),
        }
    }

    /// Scope check + dispatch + audit. Shared by the macro-generated tool methods so
    /// the wire boundary and the tests cover the same code path.
    pub async fn run(
        &self,
        tool: &str,
        arguments: serde_json::Value,
        cmd: AgentCommand,
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
        // the emitted event and the actor snapshot agree.
        let spawn_event = match (&cmd, validate_only) {
            (AgentCommand::Add(a), false) => Some(AgentEvent::Spawned {
                session: a.id.clone(),
                rig: a.rig.clone(),
                role: SessionRole::Polecat,
                crew: None,
            }),
            _ => None,
        };

        let domain_result = if validate_only {
            self.inner.agent.validate(cmd).await
        } else {
            self.inner.agent.exec(cmd).await
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

    /// Merge-domain twin of [`McpService::run`]: same scope + audit boundary, dispatching to
    /// the merge actor instead of the agent. Kept as a sibling (not a generic) so each domain's
    /// dispatch stays a flat, readable method — the registry grows one `run_*` per domain as
    /// the epic retrofits them.
    pub async fn run_merge(
        &self,
        tool: &str,
        arguments: serde_json::Value,
        cmd: MergeCommand,
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

        let domain_result = if validate_only {
            self.inner.merge.validate(cmd).await
        } else {
            self.inner.merge.exec(cmd).await
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

        match domain_result {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text("ok")])),
            Err(err) => Err(McpError::internal_error(err.to_string(), None)),
        }
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

    #[tool(
        name = "merge.start.validate",
        description = "Check whether starting a merge (Ready -> Merging) would be accepted. No state change."
    )]
    async fn merge_start_validate(
        &self,
        Parameters(args): Parameters<StartMerge>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("StartMerge is Serialize");
        self.run_merge("merge.start.validate", json, MergeCommand::Start(args), true).await
    }

    #[tool(
        name = "merge.start.execute",
        description = "Advance a merge slot Ready -> Merging. Illegal transitions are rejected."
    )]
    async fn merge_start_execute(
        &self,
        Parameters(args): Parameters<StartMerge>,
    ) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_value(&args).expect("StartMerge is Serialize");
        self.run_merge("merge.start.execute", json, MergeCommand::Start(args), false).await
    }

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

    /// Scheduling-domain twin of [`McpService::run`]: same scope + audit boundary, dispatching
    /// to the dispatcher actor instead of the agent. A sibling per domain (not a generic) so
    /// each dispatch stays a flat, readable method — the registry grows one `run_*` per domain.
    pub async fn run_sched(
        &self,
        tool: &str,
        arguments: serde_json::Value,
        cmd: SchedCommand,
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

        let domain_result = if validate_only {
            self.inner.sched.validate(cmd).await
        } else {
            self.inner.sched.exec(cmd).await
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

        match domain_result {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text("ok")])),
            Err(err) => Err(McpError::internal_error(err.to_string(), None)),
        }
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
                Ok(()) => self.inner.sched.create_bead(args.to_bead()).await,
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

    /// Scope check + (validate | `gt rig add`) + audit for `rig.create` (hq-mc72.11). Rigs are
    /// a `gt` CLI concept, not orchestrator state, so this runs the external CLI through
    /// [`RigCreator`] and emits no domain event — only the frontier audit. `execute` fails
    /// cleanly when no [`RigCreator`] is wired (tests / a server started without `GT_BIN`).
    pub async fn run_rig_create(
        &self,
        tool: &str,
        args: CreateRig,
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

        let json = serde_json::to_value(&args).expect("CreateRig is Serialize");
        let domain_result: Result<String, AppError> = if validate_only {
            args.validate().map(|()| "ok".to_string())
        } else {
            match (args.validate(), &self.inner.rig_creator) {
                (Ok(()), Some(creator)) => creator.create(&args).await,
                (Ok(()), None) => Err(AppError::Other(
                    "rig creation not configured (no GT_BIN on this server)".into(),
                )),
                (Err(e), _) => Err(e),
            }
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
            Ok(text) => Ok(CallToolResult::success(vec![Content::text(text)])),
            Err(err) => Err(McpError::internal_error(err.to_string(), None)),
        }
    }

    #[tool(
        name = "rig.create.validate",
        description = "Check whether creating a rig would be accepted (name charset + non-empty git_url). No state change."
    )]
    async fn rig_create_validate(
        &self,
        Parameters(args): Parameters<CreateRig>,
    ) -> Result<CallToolResult, McpError> {
        self.run_rig_create("rig.create.validate", args, true).await
    }

    #[tool(
        name = "rig.create.execute",
        description = "Create a Gas Town rig by cloning a repo (runs `gt rig add <name> <git_url>`). Not event-logged."
    )]
    async fn rig_create_execute(
        &self,
        Parameters(args): Parameters<CreateRig>,
    ) -> Result<CallToolResult, McpError> {
        self.run_rig_create("rig.create.execute", args, false).await
    }

    /// Patrol-domain twin of [`McpService::run`]: same scope + audit boundary, dispatching to
    /// the lease-tracker actor. A `Tick` may emit several `LeaseExpired`s; the actor relays
    /// them — `run_patrol` only reports the single ok/err the dispatch returned.
    pub async fn run_patrol(
        &self,
        tool: &str,
        arguments: serde_json::Value,
        cmd: PatrolCommand,
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

        let domain_result = if validate_only {
            self.inner.patrol.validate(cmd).await
        } else {
            self.inner.patrol.exec(cmd).await
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

        match domain_result {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text("ok")])),
            Err(err) => Err(McpError::internal_error(err.to_string(), None)),
        }
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

    /// Orchestration-domain twin of [`McpService::run`]: same scope + audit boundary,
    /// dispatching to the convoy actor. A launch/complete can emit several events (the
    /// handoff); the actor relays them — `run_orch` reports the single ok/err of the dispatch.
    pub async fn run_orch(
        &self,
        tool: &str,
        arguments: serde_json::Value,
        cmd: OrchCommand,
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

        let domain_result = if validate_only {
            self.inner.orch.validate(cmd).await
        } else {
            self.inner.orch.exec(cmd).await
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

        match domain_result {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text("ok")])),
            Err(err) => Err(McpError::internal_error(err.to_string(), None)),
        }
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

    /// Quota-domain twin of [`McpService::run`]: same scope + audit boundary, dispatching to
    /// the quota actor.
    pub async fn run_quota(
        &self,
        tool: &str,
        arguments: serde_json::Value,
        cmd: QuotaCommand,
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

        let domain_result = if validate_only {
            self.inner.quota.validate(cmd).await
        } else {
            self.inner.quota.exec(cmd).await
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

        match domain_result {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text("ok")])),
            Err(err) => Err(McpError::internal_error(err.to_string(), None)),
        }
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
            self.inner.quota.upsert_account(args.to_account()).await;
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
        ]
    }

    /// Read one snapshot resource as JSON. Unknown uri → `NotFound`. Pure read: it only
    /// snapshots the shared actors, never mutates.
    pub async fn read_resource_json(&self, uri: &str) -> Result<serde_json::Value, AppError> {
        match uri {
            "gt://agent/sessions" => {
                let sessions = self.inner.sessions.snapshot().await;
                serde_json::to_value(&sessions)
                    .map_err(|e| AppError::Other(format!("encode sessions: {e}")))
            }
            "gt://scheduling/queue" => {
                let (queued, in_flight) = self.inner.sched.snapshot().await;
                Ok(serde_json::json!({ "queued": queued, "in_flight": in_flight }))
            }
            "gt://patrol/leases" => {
                let (live_leases, expired_emitted) = self.inner.patrol.snapshot().await;
                Ok(serde_json::json!({ "live_leases": live_leases, "expired_emitted": expired_emitted }))
            }
            "gt://merge/slots" => {
                let slots = self.inner.merge.snapshot().await;
                let arr: Vec<serde_json::Value> = slots
                    .iter()
                    .map(|s| serde_json::json!({ "bead": s.bead, "branch": s.branch, "state": s.state.as_str() }))
                    .collect();
                Ok(serde_json::Value::Array(arr))
            }
            "gt://orch/convoys" => {
                let convoys = self.inner.orch.snapshot().await;
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
                let (accounts, predictions_emitted) = self.inner.quota.snapshot().await;
                Ok(serde_json::json!({ "accounts": accounts, "predictions_emitted": predictions_emitted }))
            }
            other => Err(AppError::NotFound(format!("resource {other}"))),
        }
    }
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
