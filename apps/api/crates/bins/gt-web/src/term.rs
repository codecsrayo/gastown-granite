//! `hq-fe-term.2` — WebSocket route for the dashboard dock terminal.
//!
//! `GET /api/sessions/:id/term` upgrades to a binary WebSocket that bridges a live tmux
//! pane to xterm.js. Bytes flow both directions:
//!
//! - **Pane -> client**: a `spawn_blocking` task drives the [`TerminalReader`] half of the
//!   [`Attach`] and forwards each chunk over an mpsc channel to an async sender task,
//!   which wraps it in a binary WS frame. We use a blocking thread because
//!   [`TerminalReader::read_chunk`] is synchronous (the underlying fifo / pty read
//!   blocks); putting it on the runtime's blocking pool keeps the async reactor free.
//! - **Client -> pane**: the WS receive loop forwards binary frames to
//!   [`TerminalWriter::write_keys`] and JSON text frames of shape
//!   `{"resize":{"cols":N,"rows":M}}` to [`TerminalWriter::resize`].
//!
//! Teardown is symmetric: a client disconnect closes the writer, which (for tmux)
//! triggers `pipe-pane` off so `cat` exits and the fifo gets EOF, unblocking the reader.
//! Conversely a pane EOF closes the mpsc channel, the sender task ends, and the WS gets
//! a close frame.
//!
//! Scope is `terminal.attach`; the route lives on a per-method router so a future POST
//! variant on the same path could carry a different scope without dragging both into one
//! `MethodRouter` layer (see [[feedback-axum-method-router-layer]]).

use std::sync::Arc;

use axum::body::Body;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Path, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use futures::stream::StreamExt;
use futures::SinkExt;
use serde::Deserialize;
use tokio::sync::mpsc;

use gt_agent::SessionQueries;
use gt_beads::BeadRepository;
use gt_merge::MergeRepository;
use gt_terminal::{Attach, AttachHandle, TerminalTarget};

use crate::state::AppState;

/// `GET /api/sessions/:id/term`. Returns 503 when the attach adapter is not wired (deploy
/// without `GT_TERMINAL_ENABLE=1`), 404 when the tmux session does not exist, and a 101
/// upgrade otherwise. The session id is taken verbatim as the tmux session name — same
/// shape `DELETE /api/sessions/:id` (hq-fe-api-w.6) targets.
pub async fn term_attach<R, SQ, M>(
    State(state): State<AppState<R, SQ, M>>,
    Path(session_id): Path<String>,
    req: Request<Body>,
) -> Response
where
    R: BeadRepository + Send + Sync + 'static,
    SQ: SessionQueries + Send + Sync + 'static,
    M: MergeRepository + Send + Sync + 'static,
{
    // 503 check runs BEFORE the WebSocketUpgrade extractor. A plain GET (no Upgrade
    // header) on an unwired deploy must surface "terminal disabled" with 503 rather
    // than the extractor's generic 400 "expected websocket upgrade".
    let Some(attach) = state.terminal_attach.clone() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "terminal attach not wired (set GT_TERMINAL_ENABLE=1)",
        )
            .into_response();
    };
    let (mut parts, _body) = req.into_parts();
    let ws = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
        Ok(ws) => ws,
        Err(rej) => return rej.into_response(),
    };
    ws.on_upgrade(move |socket| run_attach(socket, attach, session_id))
        .into_response()
}

/// Resize control frame body. Shape: `{"resize":{"cols":N,"rows":M}}`. Reserving text
/// frames for control keeps the binary channel pure pane bytes so xterm.js can pipe them
/// straight into its decoder without inspecting each frame.
#[derive(Debug, Deserialize)]
struct ControlFrame {
    resize: Option<ResizePayload>,
}

#[derive(Debug, Deserialize)]
struct ResizePayload {
    cols: u16,
    rows: u16,
}

fn parse_resize(text: &str) -> Option<(u16, u16)> {
    let frame: ControlFrame = serde_json::from_str(text).ok()?;
    let r = frame.resize?;
    Some((r.cols, r.rows))
}

async fn run_attach(socket: WebSocket, attach: Arc<dyn Attach>, session_id: String) {
    let target = TerminalTarget::tmux(session_id.clone());
    let handle = match attach.open(&target) {
        Ok(h) => h,
        Err(e) => {
            // 1011 = Internal Error; reason carries the AttachError text so the client
            // can surface "session not found" vs a generic upgrade failure.
            let frame = CloseFrame {
                code: 1011,
                reason: format!("attach failed: {e}").into(),
            };
            let (mut tx, _rx) = socket.split();
            let _ = tx.send(Message::Close(Some(frame))).await;
            return;
        }
    };
    let AttachHandle {
        mut reader,
        writer,
    } = handle;
    let (mut sink, mut stream) = socket.split();

    // Pane bytes pipeline: blocking reader -> bounded mpsc -> async sender. Bound = 64
    // chunks; if the client backs up far enough to fill it, the blocking reader stalls
    // (intentional backpressure) and the next pane write to tmux will block in turn.
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(64);
    let writer_for_reader = writer.clone();
    let reader_task = tokio::task::spawn_blocking(move || {
        let mut buf = vec![0u8; 4096];
        loop {
            match reader.read_chunk(&mut buf) {
                Ok(0) => break, // EOF: pane closed.
                Ok(n) => {
                    if out_tx.blocking_send(buf[..n].to_vec()).is_err() {
                        // Async sender gone — client disconnected.
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        // EOF or error: tear the writer down so the receive loop's next write_keys
        // call returns BrokenPipe and the receive loop exits.
        writer_for_reader.close();
    });

    let sender_task = tokio::spawn(async move {
        while let Some(bytes) = out_rx.recv().await {
            if sink.send(Message::Binary(bytes)).await.is_err() {
                break;
            }
        }
        let _ = sink.send(Message::Close(None)).await;
    });

    // Client-to-pane loop. Runs inline; exits on a Close frame, a read error, or a
    // failed write_keys (writer was closed by the reader task on pane EOF).
    while let Some(msg) = stream.next().await {
        let Ok(msg) = msg else { break };
        match msg {
            Message::Binary(bytes) => {
                if writer.write_keys(&bytes).is_err() {
                    break;
                }
            }
            Message::Text(t) => {
                if let Some((cols, rows)) = parse_resize(&t) {
                    let _ = writer.resize(cols, rows);
                }
                // Unknown text frames are ignored — leaves room for future control verbs
                // without breaking older clients.
            }
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) => {
                // axum/tungstenite handles ping/pong automatically at the protocol layer;
                // any payload we see here is already echoed.
            }
        }
    }

    // Teardown order:
    // 1. close writer -> tmux pipe-pane off -> cat exits -> reader fifo EOF.
    // 2. reader_task observes EOF, exits naturally; aborting it would only matter on a
    //    pty target where pipe-pane semantics don't apply, and even there the dropped
    //    Arc<dyn Attach> will eventually take the child with it.
    // 3. sender_task: out_rx ends when reader_task drops out_tx, so it exits on its own.
    writer.close();
    let _ = reader_task.await;
    let _ = sender_task.await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_resize_extracts_cols_rows() {
        assert_eq!(
            parse_resize(r#"{"resize":{"cols":120,"rows":40}}"#),
            Some((120, 40))
        );
    }

    #[test]
    fn parse_resize_ignores_unknown_text_frames() {
        assert_eq!(parse_resize(r#"{"hello":"world"}"#), None);
        assert_eq!(parse_resize("not json"), None);
        assert_eq!(parse_resize(""), None);
    }

    #[test]
    fn parse_resize_requires_both_fields() {
        // serde rejects the partial payload — missing `rows` -> None.
        assert_eq!(parse_resize(r#"{"resize":{"cols":120}}"#), None);
    }
}
