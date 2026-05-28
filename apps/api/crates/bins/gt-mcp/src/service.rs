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

use gt_events::AppError;

use gt_agent::actor::AgentHandle;
use gt_agent::{AddSession, AgentCommand, RemoveSession, Session, SessionQueries, TransitionSession};
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
use gt_quota::{ProbeWindow, QuotaCommand, RotateAccount, SampleTokens};

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
        Self::with_sessions(agent, sessions, merge, sched, patrol, orch, quota, scope, audit)
    }

    /// Same as [`McpService::new`] but lets the caller override the sessions read-side
    /// (Paso 6.h epic A, hq-u955). `main.rs` passes [`SessionsRead::Dolt`] when
    /// `GT_DOLT_URL` is set; tests keep the default actor backend via [`McpService::new`].
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
