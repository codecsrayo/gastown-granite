//! `Attach` port + value types.
//!
//! Narrow trait surface: one factory method ([`Attach::open`]) returning a duplex byte
//! stream the WS route reads/writes. Adapter authors only need to wire bytes — no protocol
//! framing, no event mapping, no scope checks live here (those are gt-web's job).

use std::io;
use thiserror::Error;

/// Identifies what to attach to.
///
/// Two shapes are supported, both opaque strings so callers don't need to know whether the
/// target lives behind tmux or a fresh pty:
///
/// - `TerminalTarget::Tmux { session }` — an existing tmux session id (the polecat name).
/// - `TerminalTarget::Spawn { program, args }` — spawn a fresh pty around a command.
///
/// Window size is advisory; adapters that can't resize (fake, pty without `SIGWINCH`
/// support) ignore the hint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalTarget {
    /// Attach to a live tmux session by id.
    Tmux {
        session: String,
        cols: u16,
        rows: u16,
    },
    /// Spawn `program` with `args` inside a fresh pty.
    Spawn {
        program: String,
        args: Vec<String>,
        cols: u16,
        rows: u16,
    },
}

impl TerminalTarget {
    /// Convenience: tmux target at the default 80x24 size.
    pub fn tmux(session: impl Into<String>) -> Self {
        Self::Tmux {
            session: session.into(),
            cols: 80,
            rows: 24,
        }
    }

    /// Convenience: spawn target at the default 80x24 size.
    pub fn spawn(program: impl Into<String>, args: Vec<String>) -> Self {
        Self::Spawn {
            program: program.into(),
            args,
            cols: 80,
            rows: 24,
        }
    }
}

/// Errors the [`Attach`] port can surface at attach time. Stream-level read/write errors are
/// returned as plain [`io::Error`] from the [`TerminalStream`] methods — those are I/O on a
/// live attach, not domain failures.
#[derive(Debug, Error)]
pub enum AttachError {
    /// The named target does not exist (no such tmux session, no such executable).
    #[error("terminal target not found: {0}")]
    NotFound(String),
    /// The adapter rejected the target shape (e.g. `TmuxPipeAttach` got a `Spawn` target).
    #[error("terminal target not supported by adapter: {0}")]
    Unsupported(String),
    /// Underlying adapter I/O failure during attach setup (mkfifo, tmux spawn, etc.).
    #[error("terminal attach io error: {0}")]
    Io(#[from] io::Error),
}

/// Factory for [`TerminalStream`]s. Implementations: [`super::TmuxPipeAttach`],
/// [`super::PtyAttach`], [`super::FakeAttach`].
pub trait Attach: Send + Sync {
    fn open(&self, target: &TerminalTarget) -> Result<Box<dyn TerminalStream>, AttachError>;
}

/// Live attach handle. Read/write/close are sync — the WS route owns the async loop.
///
/// Drop must release the underlying resources (tmux pipe-pane teardown, pty close); a leaked
/// stream would keep a fifo open or a pty child alive.
pub trait TerminalStream: Send {
    /// Read the next chunk from the target into `buf`. Returns `Ok(0)` on EOF (tmux session
    /// killed, pty child exited).
    fn read_chunk(&mut self, buf: &mut [u8]) -> io::Result<usize>;

    /// Forward `bytes` to the target as keystrokes (tmux send-keys / pty stdin write). The
    /// adapter does not encode literals — callers send raw bytes (xterm key codes for the
    /// pty path, or tmux key names for the tmux path; the WS route knows which).
    fn write_keys(&mut self, bytes: &[u8]) -> io::Result<()>;

    /// Update the advisory window size. Adapters without resize support return `Ok(())`
    /// without changing anything — the client side falls back to its initial size.
    fn resize(&mut self, _cols: u16, _rows: u16) -> io::Result<()> {
        Ok(())
    }

    /// Best-effort teardown. Called when the WS route detaches before `Drop` runs (e.g.
    /// client disconnect). Idempotent — adapters may receive `close` more than once.
    fn close(&mut self);
}
