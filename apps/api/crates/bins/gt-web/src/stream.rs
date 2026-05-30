//! Bus -> broadcast -> SSE bridge. The composition root (`bins/gt`) already broadcasts every
//! appended [`EventRecord`] (the single writer); each SSE connection here calls
//! `subscribe_events()` for its own `broadcast::Receiver`, exactly the pattern in
//! `docs/07-frontend.md`.
//!
//! Lagged subscribers (slow clients overflow the ring) get dropped frames silently — the
//! snapshot endpoint is the resync, the broadcast is best-effort live delta.

use std::convert::Infallible;

use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use futures::stream::{Stream, StreamExt};
use tokio::sync::broadcast::Receiver;
use tokio_stream::wrappers::BroadcastStream;

use gt_audit::EventRecord;

/// Build an SSE response from a fresh broadcast receiver. Each event is JSON-encoded with
/// the same shape the `.events.jsonl` log uses (`EventRecord`), so the browser and the
/// audit-log readers see byte-identical payloads — the doc's "shared `EventRecord`" rule.
pub fn sse_from_receiver(rx: Receiver<EventRecord>) -> impl IntoResponse {
    let stream = into_sse_stream(rx);
    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn into_sse_stream(
    rx: Receiver<EventRecord>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    BroadcastStream::new(rx).filter_map(|res| async move {
        match res {
            Ok(rec) => match Event::default().id(rec.event_id.clone()).json_data(&rec) {
                Ok(ev) => Some(Ok(ev)),
                Err(_) => None, // skip un-encodable frames; the log is still source of truth
            },
            // Lagged or closed: skip; client may reconnect via Last-Event-ID + snapshot.
            Err(_) => None,
        }
    })
}

/// Generic helper for SSE feeds whose payload is a plain `Serialize` type with no replay
/// identity (the `/api/worktrees/stream` feed, hq-fe-api-r.12). Same Lagged-skip posture as
/// [`sse_from_receiver`]; clients resync via the snapshot endpoint when they reconnect.
pub fn sse_from_json_receiver<T>(rx: Receiver<T>) -> impl IntoResponse
where
    T: Clone + serde::Serialize + Send + 'static,
{
    let stream = BroadcastStream::new(rx).filter_map(|res| async move {
        match res {
            Ok(payload) => match Event::default().json_data(&payload) {
                Ok(ev) => Some(Ok::<Event, Infallible>(ev)),
                Err(_) => None,
            },
            Err(_) => None,
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}
