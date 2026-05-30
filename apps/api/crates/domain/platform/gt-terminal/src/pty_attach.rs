//! [`Attach`] adapter that spawns a fresh pseudo-terminal for the target command.
//!
//! Wraps [`gt_login::pty::Pty`] so the same substrate that drives `claude /login`
//! (`hq-fe-auth.1`) backs the dashboard's ad-hoc `Spawn` attach. Only accepts
//! [`TerminalTarget::Spawn`] — tmux targets are the [`TmuxPipeAttach`](super::TmuxPipeAttach)
//! adapter's job.
//!
//! Window size from [`TerminalTarget::Spawn`] is currently ignored — `gt_login::pty::Pty`
//! fixes the pty pair at 80x24 because its only caller printed a one-line URL. A later beat
//! (`hq-fe-term.3` or its follow-up) can widen the `Pty` port; the WS route still works at
//! the default size today.

use std::io;
use std::sync::Arc;

use gt_login::pty::{Pty, PtyChild};

use crate::port::{Attach, AttachError, TerminalStream, TerminalTarget};

/// Spawn-mode [`Attach`] backed by any [`Pty`] adapter (`gt_login::pty::PortablePty` in
/// prod, `gt_login::pty::FakePty` in unit tests).
pub struct PtyAttach<P: Pty> {
    pty: Arc<P>,
}

impl<P: Pty> PtyAttach<P> {
    pub fn new(pty: P) -> Self {
        Self { pty: Arc::new(pty) }
    }

    pub fn from_arc(pty: Arc<P>) -> Self {
        Self { pty }
    }
}

impl<P: Pty + 'static> Attach for PtyAttach<P> {
    fn open(&self, target: &TerminalTarget) -> Result<Box<dyn TerminalStream>, AttachError> {
        let (program, args) = match target {
            TerminalTarget::Spawn { program, args, .. } => (program.as_str(), args.as_slice()),
            TerminalTarget::Tmux { session, .. } => {
                return Err(AttachError::Unsupported(format!(
                    "PtyAttach received tmux target {session}; use TmuxPipeAttach"
                )));
            }
        };
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let child = self.pty.spawn(program, &argv)?;
        Ok(Box::new(PtyStream { child }))
    }
}

struct PtyStream {
    child: Box<dyn PtyChild>,
}

impl TerminalStream for PtyStream {
    fn read_chunk(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.child.read_chunk(buf)
    }

    fn write_keys(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.child.write_all(bytes)
    }

    fn close(&mut self) {
        self.child.kill();
    }
}

impl Drop for PtyStream {
    fn drop(&mut self) {
        // `gt_login::PortableChild::Drop` does not kill the child by default; do it here so
        // a dropped attach stream cannot leak a live process when the WS route panics.
        self.child.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gt_login::pty::FakePty;

    #[test]
    fn rejects_tmux_target_with_unsupported() {
        let adapter = PtyAttach::new(FakePty::default());
        match adapter.open(&TerminalTarget::tmux("polecat-x")) {
            Err(AttachError::Unsupported(_)) => {}
            Err(e) => panic!("expected Unsupported, got {e:?}"),
            Ok(_) => panic!("expected Unsupported, got Ok"),
        }
    }

    #[test]
    fn spawn_target_streams_scripted_bytes() {
        let pty = FakePty::scripted(vec![b"hi".to_vec(), b" there".to_vec()], 0);
        let adapter = PtyAttach::new(pty);
        let target = TerminalTarget::spawn("/bin/echo", vec!["unused".into()]);
        let mut stream = adapter.open(&target).unwrap();
        let mut buf = [0u8; 16];
        let n = stream.read_chunk(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"hi");
        let n = stream.read_chunk(&mut buf).unwrap();
        assert_eq!(&buf[..n], b" there");
        assert_eq!(stream.read_chunk(&mut buf).unwrap(), 0);
    }

    #[test]
    fn close_kills_child_so_subsequent_reads_eof() {
        let pty = FakePty::scripted(vec![b"unread".to_vec()], 0);
        let adapter = PtyAttach::new(pty);
        let mut stream = adapter
            .open(&TerminalTarget::spawn("/bin/cat", vec![]))
            .unwrap();
        stream.close();
        let mut buf = [0u8; 8];
        assert_eq!(stream.read_chunk(&mut buf).unwrap(), 0);
    }
}
