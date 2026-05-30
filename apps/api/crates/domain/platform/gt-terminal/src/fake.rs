//! In-memory [`Attach`] for tests.
//!
//! Scripted reads + recorded writes — same shape as `gt_login::pty::FakePty`. Tests assert
//! that the WS route forwarded the right keystrokes and consumed pane bytes in order.

use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};

use crate::port::{Attach, AttachError, TerminalStream, TerminalTarget};

/// In-memory adapter. Holds one script per `open()` call; the FIFO queue lets tests stage
/// several attach lifetimes in a single fake.
#[derive(Default)]
pub struct FakeAttach {
    inner: Arc<Mutex<FakeState>>,
}

#[derive(Default)]
struct FakeState {
    /// Scripts to hand out on subsequent `open()` calls. Each script is the chunk sequence
    /// the stream will yield from `read_chunk` (one `Vec<u8>` per read).
    pending_scripts: VecDeque<Vec<Vec<u8>>>,
    /// Writes recorded across all streams this fake has opened.
    writes: Vec<Vec<u8>>,
    /// Targets seen across all opens, in attach order.
    targets: Vec<TerminalTarget>,
    /// Resize events recorded across all streams.
    resizes: Vec<(u16, u16)>,
    /// Count of `close` calls (each stream may receive more than one).
    closes: usize,
}

impl FakeAttach {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a script for the next `open()` call. Each chunk maps to one `read_chunk`
    /// invocation; the read after the last chunk returns EOF (`Ok(0)`).
    pub fn enqueue(&self, chunks: Vec<Vec<u8>>) {
        self.inner.lock().unwrap().pending_scripts.push_back(chunks);
    }

    /// Drain the keystrokes the WS route forwarded across all streams.
    pub fn take_writes(&self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.inner.lock().unwrap().writes)
    }

    /// Targets the fake was asked to attach to, in order.
    pub fn targets(&self) -> Vec<TerminalTarget> {
        self.inner.lock().unwrap().targets.clone()
    }

    /// Resize events seen across all streams, in order.
    pub fn resizes(&self) -> Vec<(u16, u16)> {
        self.inner.lock().unwrap().resizes.clone()
    }

    /// Total `close` calls across all streams.
    pub fn close_count(&self) -> usize {
        self.inner.lock().unwrap().closes
    }
}

impl Attach for FakeAttach {
    fn open(&self, target: &TerminalTarget) -> Result<Box<dyn TerminalStream>, AttachError> {
        let script = {
            let mut g = self.inner.lock().unwrap();
            g.targets.push(target.clone());
            g.pending_scripts.pop_front().unwrap_or_default()
        };
        Ok(Box::new(FakeStream {
            script: script.into(),
            shared: Arc::clone(&self.inner),
            closed: false,
        }))
    }
}

struct FakeStream {
    script: VecDeque<Vec<u8>>,
    shared: Arc<Mutex<FakeState>>,
    closed: bool,
}

impl TerminalStream for FakeStream {
    fn read_chunk(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.closed {
            return Ok(0);
        }
        let Some(next) = self.script.pop_front() else {
            return Ok(0);
        };
        let n = next.len().min(buf.len());
        buf[..n].copy_from_slice(&next[..n]);
        Ok(n)
    }

    fn write_keys(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.shared.lock().unwrap().writes.push(bytes.to_vec());
        Ok(())
    }

    fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        self.shared.lock().unwrap().resizes.push((cols, rows));
        Ok(())
    }

    fn close(&mut self) {
        if !self.closed {
            self.closed = true;
            self.shared.lock().unwrap().closes += 1;
        }
    }
}

impl Drop for FakeStream {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_records_target_and_scripts_reads() {
        let fake = FakeAttach::new();
        fake.enqueue(vec![b"hello".to_vec(), b" world".to_vec()]);

        let target = TerminalTarget::tmux("polecat-1");
        let mut stream = fake.open(&target).unwrap();

        let mut buf = [0u8; 32];
        let n = stream.read_chunk(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"hello");
        let n = stream.read_chunk(&mut buf).unwrap();
        assert_eq!(&buf[..n], b" world");
        // Script drained -> EOF.
        assert_eq!(stream.read_chunk(&mut buf).unwrap(), 0);
        assert_eq!(fake.targets(), vec![target]);
    }

    #[test]
    fn write_keys_records_in_order() {
        let fake = FakeAttach::new();
        fake.enqueue(vec![]);
        let mut stream = fake.open(&TerminalTarget::tmux("p")).unwrap();
        stream.write_keys(b"ls\n").unwrap();
        stream.write_keys(b"\x1b").unwrap(); // ESC chord
        assert_eq!(
            fake.take_writes(),
            vec![b"ls\n".to_vec(), b"\x1b".to_vec()]
        );
    }

    #[test]
    fn resize_and_close_recorded_idempotent() {
        let fake = FakeAttach::new();
        fake.enqueue(vec![]);
        let mut stream = fake.open(&TerminalTarget::tmux("p")).unwrap();
        stream.resize(120, 40).unwrap();
        stream.close();
        stream.close(); // idempotent — count still 1
        drop(stream); // Drop also calls close — still 1
        assert_eq!(fake.resizes(), vec![(120, 40)]);
        assert_eq!(fake.close_count(), 1);
    }

    #[test]
    fn read_after_close_returns_eof() {
        let fake = FakeAttach::new();
        fake.enqueue(vec![b"unread".to_vec()]);
        let mut stream = fake.open(&TerminalTarget::tmux("p")).unwrap();
        stream.close();
        let mut buf = [0u8; 16];
        assert_eq!(stream.read_chunk(&mut buf).unwrap(), 0);
    }
}
