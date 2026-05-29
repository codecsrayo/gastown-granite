//! Production [`Effects`] adapter.
//!
//! Both methods are edge I/O; the pure core stays unchanged. The trait itself is sync so the
//! reactor stays in the single async select-loop without blocking; each call spawns a tokio
//! task that does the actual work and reports back via `eprintln!` (the same channel
//! `LogEffects` uses; tracing is a follow-up).
//!
//! - `sling(convoy, member)` spawns a **real Rust-managed polecat** through
//!   [`gt_polecat::PolecatLifecycle`] (a detached `tmux` session running the coding agent with
//!   the slung bead pinned as `GT_HOOK_BEAD`). This replaced the old `gt sling` subprocess
//!   (hq-mc72.12 D1): the orchestrator no longer execs the Go binary to dispatch work.
//! - `rotate(account)` runs the [`QuotaCommand::Rotate`] chain. The quota actor holds the
//!   only authoritative registry, so the adapter snapshots accounts via [`QuotaHandle::accounts`],
//!   picks a healthy target distinct from the source, stamps `now_secs` at the edge and calls
//!   `exec`.
//!
//! The quota handle is only known after `spawn` has built the actors; the adapter therefore
//! reads it through an [`Arc<OnceLock<QuotaHandle>>`] that the bin fills in right after spawn.
//! A rotation that fires before the handle is installed is a no-op with a stderr warning —
//! the audit log still has the upstream `QuotaEvent::BlockPredicted`/`AccountLimited`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::OnceCell;

use gt_polecat::{
    PolecatLifecycle, PolecatSupervisor, RestartConfig, SpawnTemplate, TmuxCli,
};
use gt_quota::{Account, AccountQuotaStatus, QuotaCommand, QuotaHandle, RotateAccount};

use crate::root::Effects;

/// Shared, write-once slot for the quota handle. Built by [`RealEffects::new`] so the bin can
/// fill it after `spawn` returns.
pub type QuotaSlot = Arc<OnceCell<QuotaHandle>>;

/// Real [`Effects`] adapter: spawns Rust-managed polecats (via [`PolecatLifecycle`]), keeps them
/// alive through a [`PolecatSupervisor`], and drives the predictive rotation chain via the quota
/// actor.
pub struct RealEffects {
    lifecycle: Arc<PolecatLifecycle>,
    /// hq-mc72.12 C5 — every slung polecat is registered here so a dead tmux session is
    /// re-slung. `release` (Effects) unwatches when the work terminates. The bin drives the
    /// supervision pass (`tick`) on a timer and shares this same `Arc`.
    supervisor: Arc<PolecatSupervisor>,
    quota: QuotaSlot,
}

impl RealEffects {
    /// Build a new adapter from a caller-supplied polecat spawner + supervisor, together with
    /// the quota slot the bin must fill in after `spawn` returns.
    pub fn new(
        lifecycle: PolecatLifecycle,
        supervisor: Arc<PolecatSupervisor>,
    ) -> (Self, QuotaSlot) {
        let slot: QuotaSlot = Arc::new(OnceCell::new());
        (
            Self {
                lifecycle: Arc::new(lifecycle),
                supervisor,
                quota: slot.clone(),
            },
            slot,
        )
    }

    /// Production constructor: a real `tmux` edge adapter plus a [`SpawnTemplate`] sourced from
    /// the environment. This is what the bins wire — it replaces the old `gt sling` subprocess
    /// path (hq-mc72.12 D1), so the running orchestrator no longer depends on the Go binary to
    /// dispatch work. Returns the shared [`PolecatSupervisor`] so the bin can drive its
    /// supervision pass on a timer (hq-mc72.12 C5).
    pub fn from_env() -> (Self, QuotaSlot, Arc<PolecatSupervisor>) {
        let lifecycle =
            PolecatLifecycle::new(Box::new(TmuxCli::new()), spawn_template_from_env());
        // The supervisor uses its own `tmux` handle (same default server, so it observes the
        // sessions the lifecycle created). Hard restart cap from GT_POLECAT_MAX_RESTARTS.
        let max_restarts = std::env::var("GT_POLECAT_MAX_RESTARTS")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(10);
        let supervisor = Arc::new(PolecatSupervisor::new(
            Arc::new(TmuxCli::new()),
            RestartConfig::default(),
            max_restarts,
        ));
        let (effects, slot) = Self::new(lifecycle, supervisor.clone());
        (effects, slot, supervisor)
    }
}

/// Build the production polecat [`SpawnTemplate`] from the environment. Every field has a sane
/// default so the bin boots on a bare host; deployments override via:
/// `GT_RIG` (name), `GT_RIG_PREFIX` (tmux session prefix), `GT_RIG_PATH` (workdir),
/// `GT_POLECAT_CMD` (agent command), `GT_POLECAT_ARGS` (whitespace-split), and
/// `GT_POLECAT_HEARTBEAT_DIR`. `GT_ROLE`/`GT_RIG`/`GT_RIG_PATH` are seeded into the base env
/// so every spawned polecat can reorient itself.
pub fn spawn_template_from_env() -> SpawnTemplate {
    let rig = std::env::var("GT_RIG").unwrap_or_else(|_| "gastown".to_string());
    let prefix = std::env::var("GT_RIG_PREFIX").unwrap_or_else(|_| "gt".to_string());
    let workdir =
        PathBuf::from(std::env::var("GT_RIG_PATH").unwrap_or_else(|_| "/gt".to_string()));
    let command = std::env::var("GT_POLECAT_CMD").unwrap_or_else(|_| "claude".to_string());
    let args = std::env::var("GT_POLECAT_ARGS")
        .ok()
        .map(|s| s.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();
    let heartbeat_dir = std::env::var("GT_POLECAT_HEARTBEAT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    let base_env = vec![
        ("GT_ROLE".to_string(), "polecat".to_string()),
        ("GT_RIG".to_string(), rig.clone()),
        ("GT_RIG_PATH".to_string(), workdir.to_string_lossy().into_owned()),
    ];
    SpawnTemplate {
        rig,
        prefix,
        workdir,
        command,
        args,
        base_env,
        heartbeat_dir,
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn pick_target(accounts: &[Account], from: &str) -> Option<String> {
    accounts
        .iter()
        .find(|a| a.id != from && a.status == AccountQuotaStatus::Healthy)
        .map(|a| a.id.clone())
}

impl Effects for RealEffects {
    fn sling(&self, convoy: &str, member: &str) {
        let lifecycle = self.lifecycle.clone();
        let supervisor = self.supervisor.clone();
        let convoy = convoy.to_string();
        let member = member.to_string();
        // `tmux` calls are synchronous (`std::process`), so run the spawn off the async worker
        // to keep the reactor's select-loop non-blocking. `member` is the slung bead — the
        // lifecycle pins it as GT_HOOK_BEAD inside the new session (hq-63az / gg-0nb). On a
        // successful spawn the polecat is registered with the supervisor so a dead session is
        // re-slung until the work completes (hq-mc72.12 C5).
        tokio::task::spawn_blocking(move || match lifecycle.sling(&convoy, &member) {
            Ok(spec) => {
                eprintln!(
                    "[gt] slung polecat session={} rig={} hook={member} (convoy={convoy})",
                    spec.session, spec.rig
                );
                supervisor.watch(spec);
            }
            Err(e) => {
                eprintln!("[gt] sling failed convoy={convoy} member={member}: {e}")
            }
        });
    }

    fn release(&self, member: &str) {
        // Work for `member` terminated (merged or failed) — stop supervising its polecat so a
        // completed session is never resurrected (hq-mc72.12 C5).
        self.supervisor.unwatch_member(member);
    }

    fn rotate(&self, account: &str) {
        let from = account.to_string();
        let slot = self.quota.clone();
        tokio::spawn(async move {
            let Some(quota) = slot.get().cloned() else {
                eprintln!("[gt] rotate skipped: quota handle not yet installed (account={from})");
                return;
            };
            let accounts = quota.accounts().await;
            let Some(to_account) = pick_target(&accounts, &from) else {
                eprintln!("[gt] rotate skipped: no healthy target for account={from}");
                return;
            };
            let cmd = QuotaCommand::Rotate(RotateAccount {
                from_account: from.clone(),
                to_account: to_account.clone(),
                now_secs: now_secs(),
            });
            if let Err(e) = quota.exec(cmd).await {
                eprintln!("[gt] rotate exec failed from={from} to={to_account}: {e}");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gt_quota::Account;

    #[test]
    fn picks_first_healthy_distinct_target() {
        let accounts = vec![
            Account::new("a"),
            Account::new("b"),
            Account::new("c"),
        ];
        assert_eq!(pick_target(&accounts, "a").as_deref(), Some("b"));
    }

    #[test]
    fn skips_self_and_non_healthy() {
        let mut bad = Account::new("b");
        bad.status = AccountQuotaStatus::Limited;
        let accounts = vec![Account::new("a"), bad, Account::new("c")];
        assert_eq!(pick_target(&accounts, "a").as_deref(), Some("c"));
    }

    #[test]
    fn no_target_returns_none() {
        let accounts = vec![Account::new("a")];
        assert!(pick_target(&accounts, "a").is_none());
    }
}
