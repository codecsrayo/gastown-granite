//! Production [`Effects`] adapter.
//!
//! Both methods are edge I/O; the pure core stays unchanged. The trait itself is sync so the
//! reactor stays in the single async select-loop without blocking; each call spawns a tokio
//! task that does the actual work and reports back via `eprintln!` (the same channel
//! `LogEffects` uses; tracing is a follow-up).
//!
//! - `sling(convoy, member)` spawns `gt sling <convoy> <member>` as a child process. The path
//!   to the `gt` binary is configurable so deployments can swap binaries (and tests can point
//!   at a stub script that records the launch).
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
use std::process::Stdio;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::OnceCell;

use gt_quota::{Account, AccountQuotaStatus, QuotaCommand, QuotaHandle, RotateAccount};

use crate::root::Effects;

/// Shared, write-once slot for the quota handle. Built by [`RealEffects::new`] so the bin can
/// fill it after `spawn` returns.
pub type QuotaSlot = Arc<OnceCell<QuotaHandle>>;

/// Real [`Effects`] adapter: launches `gt sling` subprocesses and drives the predictive
/// rotation chain via the quota actor.
pub struct RealEffects {
    gt_bin: PathBuf,
    quota: QuotaSlot,
}

impl RealEffects {
    /// Build a new adapter together with the quota slot the bin must fill in after `spawn`.
    pub fn new(gt_bin: PathBuf) -> (Self, QuotaSlot) {
        let slot: QuotaSlot = Arc::new(OnceCell::new());
        (
            Self {
                gt_bin,
                quota: slot.clone(),
            },
            slot,
        )
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
        let gt_bin = self.gt_bin.clone();
        let convoy = convoy.to_string();
        let member = member.to_string();
        tokio::spawn(async move {
            let mut cmd = Command::new(&gt_bin);
            cmd.arg("sling")
                .arg(&convoy)
                .arg(&member)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            match cmd.spawn() {
                Ok(mut child) => {
                    if let Some(out) = child.stdout.take() {
                        let tag = format!("[gt sling/{convoy}/{member}/out]");
                        tokio::spawn(drain(tag, out));
                    }
                    if let Some(err) = child.stderr.take() {
                        let tag = format!("[gt sling/{convoy}/{member}/err]");
                        tokio::spawn(drain(tag, err));
                    }
                    match child.wait().await {
                        Ok(status) => eprintln!(
                            "[gt] sling convoy={convoy} member={member} exit={status}"
                        ),
                        Err(e) => eprintln!(
                            "[gt] sling convoy={convoy} member={member} wait error: {e}"
                        ),
                    }
                }
                Err(e) => eprintln!(
                    "[gt] sling spawn failed convoy={convoy} member={member} bin={}: {e}",
                    gt_bin.display()
                ),
            }
        });
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

async fn drain<R>(tag: String, reader: R)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buf = BufReader::new(reader).lines();
    while let Ok(Some(line)) = buf.next_line().await {
        eprintln!("{tag} {line}");
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
