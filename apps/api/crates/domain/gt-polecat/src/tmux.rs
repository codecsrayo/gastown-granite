//! Tmux edge adapter behind a [`Tmux`] port.
//!
//! Production polecats are a coding agent running inside a detached tmux session; the
//! supervisor needs to create that session with the right environment (notably
//! [`crate::GT_HOOK_BEAD`]) and to read it back. The domain depends only on the [`Tmux`]
//! trait; [`TmuxCli`] shells out to the real `tmux` binary (port of `internal/tmux`), and
//! [`FakeTmux`] is an in-memory double so the spawn/hook logic is testable without a tmux
//! server.
//!
//! Methods are synchronous: each is a single short-lived `tmux` invocation. Callers on the
//! async edge keep them off the hot path (one call per spawn, not per tick).

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

/// The session-management surface the lifecycle needs. Implemented by [`TmuxCli`] (real) and
/// [`FakeTmux`] (tests).
pub trait Tmux: Send + Sync {
    /// Create a detached session named `session` running `command args…` in `workdir`, with
    /// `env` injected before the command starts (so the agent and its `bd` subprocesses
    /// inherit it from the start — the `-e`-flags path in the Go adapter).
    fn new_session(
        &self,
        session: &str,
        workdir: &Path,
        command: &str,
        args: &[String],
        env: &[(String, String)],
    ) -> io::Result<()>;

    /// Set a single session-level environment variable after creation.
    fn set_environment(&self, session: &str, key: &str, value: &str) -> io::Result<()>;

    /// Read a session-level environment variable back. `None` when unset.
    fn show_environment(&self, session: &str, key: &str) -> io::Result<Option<String>>;

    fn has_session(&self, session: &str) -> bool;

    fn kill_session(&self, session: &str) -> io::Result<()>;
}

/// Real adapter: shells out to the `tmux` binary. Mirrors the flag shape of
/// `internal/tmux/tmux.go` (`new-session -d -s … -c … -e KEY=VAL …` then `respawn-pane`).
pub struct TmuxCli {
    bin: String,
    /// Optional `-L <socket>` server socket. Lets a caller (notably tests) run against a
    /// private tmux server instead of the shared default — never disturbing live sessions.
    socket: Option<String>,
}

impl TmuxCli {
    pub fn new() -> Self {
        Self {
            bin: "tmux".to_string(),
            socket: None,
        }
    }

    /// Use a non-default tmux binary/path (kept for parity with deployments that pin it).
    pub fn with_bin(bin: impl Into<String>) -> Self {
        Self {
            bin: bin.into(),
            socket: None,
        }
    }

    /// Pin a private server socket (`tmux -L <socket>`). Isolation for tests and for
    /// deployments that segregate tmux servers per role.
    pub fn with_socket(mut self, socket: impl Into<String>) -> Self {
        self.socket = Some(socket.into());
        self
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(&self.bin);
        if let Some(socket) = &self.socket {
            cmd.arg("-L").arg(socket);
        }
        cmd
    }

    fn run(&self, args: &[&str]) -> io::Result<String> {
        let out = self.command().args(args).output()?;
        if !out.status.success() {
            return Err(io::Error::other(format!(
                "tmux {} failed: {}",
                args.first().copied().unwrap_or(""),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

impl Default for TmuxCli {
    fn default() -> Self {
        Self::new()
    }
}

impl Tmux for TmuxCli {
    fn new_session(
        &self,
        session: &str,
        workdir: &Path,
        command: &str,
        args: &[String],
        env: &[(String, String)],
    ) -> io::Result<()> {
        let workdir = workdir.to_string_lossy().into_owned();
        let mut argv: Vec<String> =
            vec!["new-session".into(), "-d".into(), "-s".into(), session.into()];
        argv.push("-c".into());
        argv.push(workdir.clone());
        // Sort env keys for deterministic invocations (matches the Go adapter).
        let mut pairs = env.to_vec();
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        for (k, v) in &pairs {
            argv.push("-e".into());
            argv.push(format!("{k}={v}"));
        }
        let argv_ref: Vec<&str> = argv.iter().map(String::as_str).collect();
        self.run(&argv_ref)?;

        // Replace the placeholder shell with the real command in the same workdir.
        let mut respawn: Vec<String> = vec![
            "respawn-pane".into(),
            "-k".into(),
            "-t".into(),
            session.into(),
            "-c".into(),
            workdir,
            command.into(),
        ];
        respawn.extend(args.iter().cloned());
        let respawn_ref: Vec<&str> = respawn.iter().map(String::as_str).collect();
        if let Err(e) = self.run(&respawn_ref) {
            let _ = self.kill_session(session);
            return Err(e);
        }
        Ok(())
    }

    fn set_environment(&self, session: &str, key: &str, value: &str) -> io::Result<()> {
        self.run(&["set-environment", "-t", session, key, value])?;
        Ok(())
    }

    fn show_environment(&self, session: &str, key: &str) -> io::Result<Option<String>> {
        let out = self.run(&["show-environment", "-t", session, key])?;
        Ok(parse_show_environment(&out, key))
    }

    fn has_session(&self, session: &str) -> bool {
        self.command()
            .args(["has-session", "-t", session])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn kill_session(&self, session: &str) -> io::Result<()> {
        self.run(&["kill-session", "-t", session])?;
        Ok(())
    }
}

/// Parse `tmux show-environment -t <s> <key>` output. tmux prints `KEY=value` when set and
/// `-KEY` (leading dash) when explicitly unset; anything else → not present.
fn parse_show_environment(out: &str, key: &str) -> Option<String> {
    for line in out.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(&format!("{key}=")) {
            return Some(rest.to_string());
        }
        if line == format!("-{key}") {
            return None;
        }
    }
    None
}

/// In-memory [`Tmux`] for tests: records sessions and their env without a tmux server.
#[derive(Default)]
pub struct FakeTmux {
    sessions: Mutex<HashMap<String, HashMap<String, String>>>,
}

impl FakeTmux {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Tmux for FakeTmux {
    fn new_session(
        &self,
        session: &str,
        _workdir: &Path,
        _command: &str,
        _args: &[String],
        env: &[(String, String)],
    ) -> io::Result<()> {
        let mut map = self.sessions.lock().unwrap();
        let entry = map.entry(session.to_string()).or_default();
        for (k, v) in env {
            entry.insert(k.clone(), v.clone());
        }
        Ok(())
    }

    fn set_environment(&self, session: &str, key: &str, value: &str) -> io::Result<()> {
        let mut map = self.sessions.lock().unwrap();
        map.entry(session.to_string())
            .or_default()
            .insert(key.to_string(), value.to_string());
        Ok(())
    }

    fn show_environment(&self, session: &str, key: &str) -> io::Result<Option<String>> {
        let map = self.sessions.lock().unwrap();
        Ok(map.get(session).and_then(|e| e.get(key).cloned()))
    }

    fn has_session(&self, session: &str) -> bool {
        self.sessions.lock().unwrap().contains_key(session)
    }

    fn kill_session(&self, session: &str) -> io::Result<()> {
        self.sessions.lock().unwrap().remove(session);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_set_and_unset() {
        assert_eq!(
            parse_show_environment("GT_HOOK_BEAD=hq-9\n", "GT_HOOK_BEAD").as_deref(),
            Some("hq-9")
        );
        assert!(parse_show_environment("-GT_HOOK_BEAD\n", "GT_HOOK_BEAD").is_none());
        assert!(parse_show_environment("OTHER=1\n", "GT_HOOK_BEAD").is_none());
    }

    #[test]
    fn fake_roundtrips_env() {
        let t = FakeTmux::new();
        t.new_session(
            "s1",
            Path::new("/tmp"),
            "claude",
            &[],
            &[("GT_HOOK_BEAD".into(), "hq-1".into())],
        )
        .unwrap();
        assert!(t.has_session("s1"));
        assert_eq!(
            t.show_environment("s1", "GT_HOOK_BEAD").unwrap().as_deref(),
            Some("hq-1")
        );
        t.kill_session("s1").unwrap();
        assert!(!t.has_session("s1"));
    }
}
