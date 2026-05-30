//! Per-actor rate-limit middleware for write-heavy routes (hq-fe-api-w.11). Fronts
//! `POST /api/beads/bulk` so a runaway script cannot empty the dispatcher's per-actor
//! budget; reused easily by other mutation routes once the kanban grows them.
//!
//! Algorithm: fixed-window counter keyed by actor namespace. The window resets on the
//! first request after `window`'s tail; while inside it, every request increments a
//! counter, and the (window+1)-th call returns 429 with a `Retry-After` header pointing
//! to the window's remaining seconds. Fixed-window is intentionally simple — a leaky
//! bucket would give smoother throttling but the bead asks for "don't let a script blow
//! up the queue", which fixed-window already covers.
//!
//! Actor derivation mirrors [`crate::idempotency::actor_namespace`]: hashed bearer
//! token, falling back to `anon`. Open-mode dev callers all share the `anon` budget so
//! tests can still exercise the 429 path without juggling tokens.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderName, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use sha2::{Digest, Sha256};

/// Default window the bead pins. Operators may override via `GT_WEB_BULK_RATE_WINDOW_SECS`.
pub const DEFAULT_WINDOW: Duration = Duration::from_secs(60);

/// Default cap per actor per window. Tuned for interactive imports (a single bulk call
/// of up to [`crate::routes::BULK_BEADS_MAX_ITEMS`] beads is the dominant pattern) —
/// raise via `GT_WEB_BULK_RATE_MAX` if a legitimate operator workflow needs more.
pub const DEFAULT_MAX_REQUESTS: u32 = 10;

const HEADER_REMAINING: HeaderName = HeaderName::from_static("x-ratelimit-remaining");

#[derive(Clone)]
struct Bucket {
    window_start: Instant,
    count: u32,
}

/// Shared per-actor counter store. Clone-cheap; the inner state lives behind an `Arc`
/// so axum's `Layer` cloning does not duplicate budgets.
#[derive(Clone)]
pub struct RateLimitStore {
    inner: Arc<Mutex<HashMap<String, Bucket>>>,
    window: Duration,
    max_requests: u32,
}

impl RateLimitStore {
    pub fn new(window: Duration, max_requests: u32) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            window,
            max_requests,
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_WINDOW, DEFAULT_MAX_REQUESTS)
    }

    pub fn window(&self) -> Duration {
        self.window
    }

    pub fn max_requests(&self) -> u32 {
        self.max_requests
    }

    /// Try to claim one slot for `actor`. Returns `Ok(remaining)` when the call fits
    /// inside the window; returns `Err(retry_after)` with the seconds until the
    /// window resets when the cap is hit.
    fn try_claim(&self, actor: &str) -> Result<u32, u64> {
        let now = Instant::now();
        let mut map = self.inner.lock().unwrap();
        let bucket = map.entry(actor.to_string()).or_insert(Bucket {
            window_start: now,
            count: 0,
        });
        if now.duration_since(bucket.window_start) >= self.window {
            bucket.window_start = now;
            bucket.count = 0;
        }
        if bucket.count >= self.max_requests {
            let elapsed = now.duration_since(bucket.window_start);
            let remaining = self.window.checked_sub(elapsed).unwrap_or(Duration::ZERO);
            let retry_after = remaining.as_secs().max(1);
            return Err(retry_after);
        }
        bucket.count += 1;
        Ok(self.max_requests - bucket.count)
    }
}

/// Axum middleware: claims a slot for the request's actor before invoking the handler.
/// On miss, short-circuits with 429 + `Retry-After`. Successful requests carry the
/// remaining budget back in `X-RateLimit-Remaining` so well-behaved clients can pace
/// themselves without round-trip guessing.
pub async fn rate_limit_middleware(
    State(store): State<RateLimitStore>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let actor = actor_namespace(req.headers());
    let remaining = match store.try_claim(&actor) {
        Ok(r) => r,
        Err(retry_after) => {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                [
                    (header::RETRY_AFTER, retry_after.to_string()),
                    (HEADER_REMAINING, "0".to_string()),
                ],
                Json(serde_json::json!({
                    "error": "rate limit exceeded",
                    "retry_after_secs": retry_after,
                })),
            )
                .into_response();
        }
    };
    let mut resp = next.run(req).await;
    if let Ok(value) = remaining.to_string().parse() {
        resp.headers_mut().insert(HEADER_REMAINING, value);
    }
    resp
}

/// Same shape as `idempotency::actor_namespace`. Kept local so the middleware does not
/// depend on a sibling module's internals — both derive the same hash from the same
/// header, which is the contract that matters.
pub fn actor_namespace(headers: &HeaderMap) -> String {
    let Some(value) = headers.get(header::AUTHORIZATION) else {
        return "anon".to_string();
    };
    let Ok(text) = value.to_str() else {
        return "anon".to_string();
    };
    let Some(secret) = text.strip_prefix("Bearer ").map(str::trim) else {
        return "anon".to_string();
    };
    if secret.is_empty() {
        return "anon".to_string();
    }
    let digest = Sha256::digest(secret.as_bytes());
    let mut out = String::with_capacity(24);
    for byte in digest.iter().take(12) {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_n_calls_inside_window_succeed() {
        let store = RateLimitStore::new(Duration::from_secs(60), 3);
        assert_eq!(store.try_claim("anon"), Ok(2));
        assert_eq!(store.try_claim("anon"), Ok(1));
        assert_eq!(store.try_claim("anon"), Ok(0));
    }

    #[test]
    fn over_cap_returns_retry_after() {
        let store = RateLimitStore::new(Duration::from_secs(60), 2);
        store.try_claim("anon").unwrap();
        store.try_claim("anon").unwrap();
        let err = store.try_claim("anon").unwrap_err();
        assert!(err > 0 && err <= 60, "retry_after within window: {err}");
    }

    #[test]
    fn separate_actors_have_independent_budgets() {
        let store = RateLimitStore::new(Duration::from_secs(60), 1);
        store.try_claim("alice").unwrap();
        // Alice exhausted, Bob is fine.
        assert!(store.try_claim("alice").is_err());
        assert!(store.try_claim("bob").is_ok());
    }

    #[test]
    fn window_resets_after_elapsed() {
        // Use a sub-millisecond window so the test can wait through it without sleeping
        // long. The Instant arithmetic is the same as the production path.
        let store = RateLimitStore::new(Duration::from_millis(50), 1);
        store.try_claim("anon").unwrap();
        assert!(store.try_claim("anon").is_err());
        std::thread::sleep(Duration::from_millis(70));
        store.try_claim("anon").expect("next window allows a fresh claim");
    }
}
