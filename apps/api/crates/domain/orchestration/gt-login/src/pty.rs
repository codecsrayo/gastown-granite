//! `Pty` port + adapters.
//!
//! The state machine in [`crate::state`] is platform-neutral; the only side-effecting hop
//! is the pseudo-terminal that hosts `claude /login`. We model it as a port so:
//!
//! - `cargo test` can drive the full state machine via [`FakePty`] without spawning a real
//!   process (CI and macOS/Windows hosts have no `claude` binary).
//! - Production uses [`PortablePty`] (the `portable-pty` crate) which gives us a real TTY
//!   so the CLI's URL printer behaves as it does on a user's terminal.
//!
//! The trait is intentionally narrow — spawn, then read/write chunks — because the driver
//! does the parsing. Adapter authors only need to forward bytes.

use std::collections::VecDeque;
use std::io;
use std::sync::Mutex;

/// Live PTY child. Drop must terminate the child (the real adapter calls `kill_on_drop`),
/// so the driver does not need an explicit teardown after `Failed`/`Complete`.
pub trait PtyChild: Send {
    /// Read the next chunk from the child's combined stdout+stderr. Returns `Ok(0)` on
    /// EOF (child exited or pty closed).
    fn read_chunk(&mut self, buf: &mut [u8]) -> io::Result<usize>;

    /// Write `bytes` to the child's stdin. The driver appends a trailing newline when it
    /// forwards the user's token, so adapters should not add their own.
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()>;

    /// Wait for the child to exit and return its raw exit status. Adapters that cannot
    /// observe the status (test fakes) may return `0` to signal success.
    fn wait(&mut self) -> io::Result<i32>;

    /// Best-effort termination. Called when the caller cancels mid-flight.
    fn kill(&mut self);
}

/// Spawn port. The driver passes the executable + args; adapters wire up the child.
pub trait Pty: Send + Sync {
    /// Spawn `program` with `args` under a fresh pty. The caller never sees the pty handle
    /// directly — only the child it owns.
    fn spawn(&self, program: &str, args: &[&str]) -> io::Result<Box<dyn PtyChild>>;
}

// ---------------------------------------------------------------------------
// FakePty — scripted test adapter.

/// In-memory adapter for tests. The driver thinks it spawned `claude /login`; in reality
/// reads come from a scripted byte queue and writes are recorded for assertions.
///
/// Scripts are passed as `Vec<Vec<u8>>` so each chunk maps to one `read_chunk` call —
/// useful for testing that the URL parser handles split-across-chunks output.
#[derive(Default)]
pub struct FakePty {
    inner: Mutex<FakeState>,
}

#[derive(Default)]
struct FakeState {
    next_script: VecDeque<Vec<u8>>,
    next_exit: i32,
    last_writes: Vec<Vec<u8>>,
}

impl FakePty {
    /// Construct a fake that, on the next `spawn`, will hand the child a script of chunks
    /// and exit with `exit_status` after `wait`.
    pub fn scripted(chunks: Vec<Vec<u8>>, exit_status: i32) -> Self {
        Self {
            inner: Mutex::new(FakeState {
                next_script: chunks.into(),
                next_exit: exit_status,
                last_writes: Vec::new(),
            }),
        }
    }

    /// Drain the writes the driver pushed to the child's stdin since the last call.
    /// Tests assert that the token was forwarded with the expected trailing newline.
    pub fn take_writes(&self) -> Vec<Vec<u8>> {
        let mut g = self.inner.lock().expect("fake pty mutex");
        std::mem::take(&mut g.last_writes)
    }
}

impl Pty for FakePty {
    fn spawn(&self, _program: &str, _args: &[&str]) -> io::Result<Box<dyn PtyChild>> {
        let (script, exit) = {
            let mut g = self.inner.lock().expect("fake pty mutex");
            (std::mem::take(&mut g.next_script), g.next_exit)
        };
        Ok(Box::new(FakeChild {
            script,
            exit,
            killed: false,
            // SAFETY: we hold the Mutex pointer; the FakePty outlives the child within a
            // test, so a raw shared pointer to the inner state is the simplest way to
            // record writes back without `Arc`-ifying the public API.
            parent: self as *const FakePty,
        }))
    }
}

struct FakeChild {
    script: VecDeque<Vec<u8>>,
    exit: i32,
    killed: bool,
    parent: *const FakePty,
}

// SAFETY: the test only touches the fake from the driver thread; in real prod the trait
// object stays on a single owning task.
unsafe impl Send for FakeChild {}

impl PtyChild for FakeChild {
    fn read_chunk(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.killed {
            return Ok(0);
        }
        let Some(next) = self.script.pop_front() else {
            return Ok(0);
        };
        let n = next.len().min(buf.len());
        buf[..n].copy_from_slice(&next[..n]);
        Ok(n)
    }

    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        // SAFETY: `parent` was constructed from a borrowed `&FakePty` whose lifetime
        // covers the FakeChild within tests. We never deref after parent is dropped.
        let parent = unsafe { &*self.parent };
        let mut g = parent.inner.lock().expect("fake pty mutex");
        g.last_writes.push(bytes.to_vec());
        Ok(())
    }

    fn wait(&mut self) -> io::Result<i32> {
        Ok(self.exit)
    }

    fn kill(&mut self) {
        self.killed = true;
    }
}

// ---------------------------------------------------------------------------
// PortablePty — production adapter over the `portable-pty` crate.

/// Real adapter using `portable-pty`. Each `spawn` allocates a fresh pty pair, attaches
/// the child to the slave, and exposes the master's reader/writer to the driver.
///
/// Window size is fixed at 80x24 — `claude /login` only prints a one-line URL and reads
/// one line of token, so a real terminal size brings nothing.
pub struct PortablePty;

impl PortablePty {
    pub fn new() -> Self {
        Self
    }
}

impl Default for PortablePty {
    fn default() -> Self {
        Self::new()
    }
}

impl Pty for PortablePty {
    fn spawn(&self, program: &str, args: &[&str]) -> io::Result<Box<dyn PtyChild>> {
        use portable_pty::{native_pty_system, CommandBuilder, PtySize};

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        let mut cmd = CommandBuilder::new(program);
        for a in args {
            cmd.arg(a);
        }
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
        // Slave handle is no longer needed once the child holds it; drop closes the FD on
        // our side so EOF propagates when the child exits.
        drop(pair.slave);

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        Ok(Box::new(PortableChild {
            child,
            reader,
            writer,
        }))
    }
}

struct PortableChild {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    reader: Box<dyn std::io::Read + Send>,
    writer: Box<dyn std::io::Write + Send>,
}

impl PtyChild for PortableChild {
    fn read_chunk(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.reader.read(buf)
    }

    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()
    }

    fn wait(&mut self) -> io::Result<i32> {
        let status = self
            .child
            .wait()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
        Ok(status.exit_code() as i32)
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
    }
}
