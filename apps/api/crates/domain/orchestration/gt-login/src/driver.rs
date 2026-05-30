//! Sync driver that wraps a [`Pty`] adapter, ingests output, and surfaces [`LoginEvent`]s
//! via a caller-supplied callback. The `gt-web` task (`hq-fe-auth.2`) will drive this from
//! a tokio `spawn_blocking` thread; the domain stays sync so it can be exercised end-to-end
//! by `#[test]` without a runtime.

use std::sync::Arc;

use crate::events::{LoginEvent, LoginFailure};
use crate::pty::{Pty, PtyChild};
use crate::state::extract_url;

/// Terminal outcome of [`LoginDriver::run`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginOutcome {
    /// Token accepted; the CLI exited 0.
    Complete { account: String },
    /// Anything that did not reach `Complete`.
    Failed { reason: LoginFailure },
}

/// What the driver needs from the caller, plumbed in through [`LoginDriver::run`]:
///
/// - the spawn-port adapter ([`Pty`])
/// - the executable + args (`("claude", &["/login"])` in prod)
/// - the account label to echo back on `Complete`
/// - a token producer: the driver blocks on this after surfacing `UrlReady`. Returning
///   `None` cancels the flow (mapped to [`LoginFailure::Cancelled`]).
/// - an event sink that takes each [`LoginEvent`] as it happens.
pub struct LoginDriver<P: Pty + ?Sized> {
    pty: Arc<P>,
}

impl<P: Pty + ?Sized> LoginDriver<P> {
    pub fn new(pty: Arc<P>) -> Self {
        Self { pty }
    }

    /// Run the login flow to completion. Blocks the current thread; callers on async
    /// edges put this on `spawn_blocking`.
    pub fn run(
        &self,
        program: &str,
        args: &[&str],
        account: &str,
        mut token_source: impl FnMut(&str) -> Option<String>,
        mut on_event: impl FnMut(LoginEvent),
    ) -> LoginOutcome {
        let mut child = match self.pty.spawn(program, args) {
            Ok(c) => c,
            Err(e) => {
                let reason = LoginFailure::Spawn {
                    message: e.to_string(),
                };
                on_event(LoginEvent::Failed {
                    reason: reason.clone(),
                });
                return LoginOutcome::Failed { reason };
            }
        };

        on_event(LoginEvent::Started);
        let mut output_buf = String::new();
        let mut chunk = [0u8; 4096];

        // Phase 1: read until we extract the URL.
        let url_for_prompt: String = loop {
            let n = match child.read_chunk(&mut chunk) {
                Ok(0) => 0,
                Ok(n) => n,
                Err(e) => return fail_io(&mut on_event, &mut *child, e),
            };
            if n == 0 {
                // EOF before URL — fatal.
                let reason = LoginFailure::UrlMissing;
                on_event(LoginEvent::Failed {
                    reason: reason.clone(),
                });
                child.kill();
                return LoginOutcome::Failed { reason };
            }
            // The CLI prints UTF-8; loosely decode so a chunk boundary in the middle of a
            // multi-byte sequence does not break URL extraction (the URL itself is ASCII).
            output_buf.push_str(&String::from_utf8_lossy(&chunk[..n]));
            if let Some(url) = extract_url(&output_buf) {
                on_event(LoginEvent::UrlReady { url: url.clone() });
                break url;
            }
        };

        // Phase 2: ask the caller for a token (this is where `gt-web` blocks on the user
        // pasting the code back through HTTP / a oneshot channel).
        let token = match token_source(&url_for_prompt) {
            Some(t) => t,
            None => {
                let reason = LoginFailure::Cancelled;
                on_event(LoginEvent::Failed {
                    reason: reason.clone(),
                });
                child.kill();
                return LoginOutcome::Failed { reason };
            }
        };

        // Phase 3: write the token + newline to stdin, wait for the child to exit.
        let mut payload = token.into_bytes();
        payload.push(b'\n');
        if let Err(e) = child.write_all(&payload) {
            return fail_io(&mut on_event, &mut *child, e);
        }

        let status = match child.wait() {
            Ok(s) => s,
            Err(e) => return fail_io(&mut on_event, &mut *child, e),
        };
        if status == 0 {
            on_event(LoginEvent::Complete {
                account: account.to_string(),
            });
            LoginOutcome::Complete {
                account: account.to_string(),
            }
        } else {
            let reason = LoginFailure::TokenRejected { status };
            on_event(LoginEvent::Failed {
                reason: reason.clone(),
            });
            LoginOutcome::Failed { reason }
        }
    }
}

/// Caller-facing handle alias (the spawn-side wrapper in `gt-web` will hold this driver
/// behind an `Arc` keyed by account). Kept as a thin re-export so the API surface stays
/// stable across `.1` / `.2`.
pub type LoginHandle<P> = LoginDriver<P>;

fn fail_io(
    on_event: &mut impl FnMut(LoginEvent),
    child: &mut dyn PtyChild,
    err: std::io::Error,
) -> LoginOutcome {
    let reason = LoginFailure::Io {
        message: err.to_string(),
    };
    on_event(LoginEvent::Failed {
        reason: reason.clone(),
    });
    child.kill();
    LoginOutcome::Failed { reason }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pty::FakePty;

    fn collect_events() -> (impl FnMut(LoginEvent), std::sync::Arc<std::sync::Mutex<Vec<LoginEvent>>>)
    {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let cloned = log.clone();
        let sink = move |e: LoginEvent| {
            cloned.lock().expect("event log").push(e);
        };
        (sink, log)
    }

    #[test]
    fn happy_path_emits_started_url_ready_complete() {
        let pty = Arc::new(FakePty::scripted(
            vec![b"Open https://console.anthropic.com/oauth?state=xyz to continue.\n".to_vec()],
            0,
        ));
        let driver = LoginDriver::new(pty.clone());
        let (sink, log) = collect_events();
        let outcome = driver.run(
            "claude",
            &["/login"],
            "primary",
            |_url| Some("TOK-123".to_string()),
            sink,
        );

        assert_eq!(
            outcome,
            LoginOutcome::Complete {
                account: "primary".into()
            }
        );
        let log = log.lock().expect("event log").clone();
        assert_eq!(log.len(), 3);
        assert_eq!(log[0], LoginEvent::Started);
        assert_eq!(
            log[1],
            LoginEvent::UrlReady {
                url: "https://console.anthropic.com/oauth?state=xyz".into()
            }
        );
        assert_eq!(
            log[2],
            LoginEvent::Complete {
                account: "primary".into()
            }
        );
        // Token must have been forwarded with the trailing newline the CLI's stdin reader
        // expects.
        assert_eq!(pty.take_writes(), vec![b"TOK-123\n".to_vec()]);
    }

    #[test]
    fn url_split_across_chunks_is_extracted() {
        // The CLI sometimes flushes the URL in two writes; the driver must accumulate
        // chunks before running the regex.
        let pty = Arc::new(FakePty::scripted(
            vec![
                b"Open https://console.anthropic".to_vec(),
                b".com/oauth?x=1\n".to_vec(),
            ],
            0,
        ));
        let driver = LoginDriver::new(pty);
        let (sink, log) = collect_events();
        let outcome = driver.run("claude", &["/login"], "alt", |_| Some("T".into()), sink);
        assert!(matches!(outcome, LoginOutcome::Complete { .. }));
        let log = log.lock().expect("event log").clone();
        assert!(matches!(
            log[1],
            LoginEvent::UrlReady { ref url } if url == "https://console.anthropic.com/oauth?x=1"
        ));
    }

    #[test]
    fn eof_before_url_is_url_missing() {
        let pty = Arc::new(FakePty::scripted(
            vec![b"banner with no url\n".to_vec()],
            0,
        ));
        let driver = LoginDriver::new(pty);
        let (sink, log) = collect_events();
        let outcome = driver.run("claude", &["/login"], "a", |_| Some("T".into()), sink);
        assert_eq!(
            outcome,
            LoginOutcome::Failed {
                reason: LoginFailure::UrlMissing
            }
        );
        let log = log.lock().expect("event log").clone();
        assert!(matches!(log.last(), Some(LoginEvent::Failed { .. })));
    }

    #[test]
    fn caller_cancel_propagates() {
        let pty = Arc::new(FakePty::scripted(
            vec![b"https://console.anthropic.com/x\n".to_vec()],
            0,
        ));
        let driver = LoginDriver::new(pty);
        let (sink, _log) = collect_events();
        let outcome = driver.run("claude", &["/login"], "a", |_| None, sink);
        assert_eq!(
            outcome,
            LoginOutcome::Failed {
                reason: LoginFailure::Cancelled
            }
        );
    }

    #[test]
    fn nonzero_exit_is_token_rejected() {
        let pty = Arc::new(FakePty::scripted(
            vec![b"https://console.anthropic.com/x\n".to_vec()],
            17,
        ));
        let driver = LoginDriver::new(pty);
        let (sink, _log) = collect_events();
        let outcome = driver.run("claude", &["/login"], "a", |_| Some("BAD".into()), sink);
        assert_eq!(
            outcome,
            LoginOutcome::Failed {
                reason: LoginFailure::TokenRejected { status: 17 }
            }
        );
    }
}
