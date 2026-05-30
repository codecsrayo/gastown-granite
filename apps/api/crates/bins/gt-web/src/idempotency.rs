//! Idempotency-Key middleware for gt-web mutation routes (hq-fe-api-w.2).
//!
//! Reads the `Idempotency-Key` request header and remembers the response for a TTL window.
//! A second call with the same key (and same actor namespace) replays the stored response
//! verbatim instead of re-running the handler, so an agent retry over a flaky network
//! cannot double-mutate the canonical state. Pairs with the `idem_key: Option<&str>` slot
//! the `gt-root::CommandBus` already threads through every domain dispatch
//! ([[project_hq_fe_api_w_cmdbus]]) — gt-web is the first frontier to consume it.
//!
//! Scope:
//! - Only mutation methods (`POST`/`PATCH`/`PUT`/`DELETE`) are subject to caching; GET/HEAD
//!   pass through unchanged so a stray header on a snapshot endpoint never wedges reads.
//! - Without the header the request reaches the handler normally; the feature is opt-in
//!   per client call.
//! - Key namespace = `{actor}:{key}` so two clients reusing the same key value land in
//!   separate cache slots. `actor` is derived from the bearer token the same way the
//!   [`super::auth::actor_tag`] computes it; `AuthConfig::Open` callers share the `anon`
//!   namespace.
//!
//! Storage:
//! - In-memory `HashMap` behind a `Mutex` with a configurable size cap. Entries carry an
//!   expiry timestamp; lookups prune their own row on miss, inserts prune the oldest
//!   entries when the cap is hit. The bead also mentions an optional Dolt-backed cache —
//!   deferred until cross-instance gt-web replay is on the table.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::{to_bytes, Body};
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use sha2::{Digest, Sha256};

/// Default TTL the bead's design pins at 10 minutes. Operators may tune via
/// `GT_WEB_IDEMPOTENCY_TTL_SECS` at boot.
pub const DEFAULT_TTL: Duration = Duration::from_secs(10 * 60);

/// Soft cap on stored entries; the bead leaves the exact bound implementation-defined.
/// Insertion past the cap drops the oldest expired entry first, then the oldest entry
/// overall if every row is still live — interactive dashboard volume keeps this rare.
pub const DEFAULT_MAX_ENTRIES: usize = 4096;

/// Header name as written by clients. `axum::http::header::HeaderName` does not ship a
/// constant for it, so we build it from a static byte string.
pub const HEADER_NAME: &str = "idempotency-key";

/// Maximum response body the middleware will buffer for replay. Larger responses skip
/// caching so a chunked SSE stream can't OOM the store; the request still mutates exactly
/// once because the handler ran.
pub const MAX_BODY_BYTES: usize = 1 << 20; // 1 MiB

/// Cached response payload. Stored as raw bytes so the middleware can rebuild the response
/// without re-encoding through the typed JSON layer.
#[derive(Clone)]
struct CachedResponse {
    status: StatusCode,
    content_type: Option<HeaderValue>,
    body: Vec<u8>,
    expires_at: Instant,
}

impl CachedResponse {
    fn to_response(&self) -> Response {
        let mut resp = Response::builder()
            .status(self.status)
            .header("x-idempotent-replay", "true");
        if let Some(ct) = &self.content_type {
            resp = resp.header(header::CONTENT_TYPE, ct.clone());
        }
        resp.body(Body::from(self.body.clone()))
            .expect("CachedResponse is well-formed")
    }
}

/// Shared cache + configuration carried as router state. Cheap to clone (the cache is
/// behind `Arc<Mutex<...>>`).
#[derive(Clone)]
pub struct IdempotencyStore {
    inner: Arc<Mutex<HashMap<String, CachedResponse>>>,
    ttl: Duration,
    max_entries: usize,
}

impl IdempotencyStore {
    pub fn new(ttl: Duration, max_entries: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            ttl,
            max_entries,
        }
    }

    /// Convenience for production wiring: 10-minute TTL + 4096-entry cap (the bead's design
    /// defaults). Operators override via env in `main.rs`.
    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_TTL, DEFAULT_MAX_ENTRIES)
    }

    fn lookup(&self, key: &str) -> Option<CachedResponse> {
        let mut guard = self.inner.lock().expect("idempotency cache poisoned");
        let entry = guard.get(key)?.clone();
        if entry.expires_at <= Instant::now() {
            guard.remove(key);
            return None;
        }
        Some(entry)
    }

    fn insert(&self, key: String, response: CachedResponse) {
        let mut guard = self.inner.lock().expect("idempotency cache poisoned");
        if guard.len() >= self.max_entries {
            // Cheapest prune: drop every expired row first; if still over the cap, evict
            // the row closest to expiry overall. O(N) but N is bounded by `max_entries`.
            let now = Instant::now();
            guard.retain(|_, v| v.expires_at > now);
            if guard.len() >= self.max_entries {
                if let Some((oldest_key, _)) = guard
                    .iter()
                    .min_by_key(|(_, v)| v.expires_at)
                    .map(|(k, v)| (k.clone(), v.clone()))
                {
                    guard.remove(&oldest_key);
                }
            }
        }
        guard.insert(key, response);
    }

    /// Visible to tests; the prod `idempotency_middleware` derives ttl from `self.ttl`.
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// Visible to tests.
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .expect("idempotency cache poisoned")
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for IdempotencyStore {
    fn default() -> Self {
        Self::with_defaults()
    }
}

/// Returns true when this method may mutate state and is therefore subject to replay.
fn is_mutation(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PATCH | Method::PUT | Method::DELETE
    )
}

/// Pull the `Idempotency-Key` value out of the request headers. Empty values are treated
/// the same as a missing header so a misconfigured client cannot collide on `""`.
fn extract_key(headers: &HeaderMap) -> Option<String> {
    let header = headers.get(HEADER_NAME)?;
    let value = header.to_str().ok()?.trim();
    if value.is_empty() {
        return None;
    }
    Some(value.to_string())
}

/// Derive an actor namespace from the request's bearer token, falling back to `anon`. We
/// hash here instead of trusting the auth layer to inject an extension so the middleware
/// stays usable behind `AuthConfig::Open` (tests, dev) without coupling.
fn actor_namespace(headers: &HeaderMap) -> String {
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
    let mut h = Sha256::new();
    h.update(secret.as_bytes());
    let digest = h.finalize();
    digest.iter().take(6).map(|b| format!("{:02x}", b)).collect()
}

/// Axum middleware entry point. Wire via
/// `Router::layer(middleware::from_fn_with_state(store, idempotency_middleware))`.
pub async fn idempotency_middleware(
    State(store): State<IdempotencyStore>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if !is_mutation(req.method()) {
        return next.run(req).await;
    }
    let Some(key) = extract_key(req.headers()) else {
        return next.run(req).await;
    };
    let namespaced = format!("{}:{}", actor_namespace(req.headers()), key);

    if let Some(cached) = store.lookup(&namespaced) {
        return cached.to_response();
    }

    let response = next.run(req).await;
    let status = response.status();
    let content_type = response.headers().get(header::CONTENT_TYPE).cloned();
    let (parts, body) = response.into_parts();
    let bytes = match to_bytes(body, MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(_) => {
            // Response body larger than MAX_BODY_BYTES — skip caching but still surface a
            // generic error to the caller. The handler already ran; the only honest answer
            // is to fail this request so the agent retries with no cache poisoning.
            return Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::from(
                    "idempotency: response body too large to cache",
                ))
                .expect("static error response is well-formed");
        }
    };
    let cached = CachedResponse {
        status,
        content_type,
        body: bytes.to_vec(),
        expires_at: Instant::now() + store.ttl,
    };
    store.insert(namespaced, cached.clone());

    let mut resp = Response::from_parts(parts, Body::from(bytes));
    // Make the header order match `to_response()` so cache-hits and live responses look
    // identical to the consumer modulo `x-idempotent-replay`.
    resp.headers_mut().remove("x-idempotent-replay");
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::{get, post};
    use axum::Router;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn counting_router(store: IdempotencyStore) -> (Router, Arc<AtomicUsize>) {
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_for_handler = counter.clone();
        let router = Router::new()
            .route(
                "/mutate",
                post(move || {
                    let counter = counter_for_handler.clone();
                    async move {
                        let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
                        (StatusCode::CREATED, format!("call {n}"))
                    }
                }),
            )
            .route("/read", get(|| async { "snapshot" }))
            .layer(axum::middleware::from_fn_with_state(
                store,
                idempotency_middleware,
            ));
        (router, counter)
    }

    async fn run(
        router: &mut Router,
        method: Method,
        path: &str,
        idem: Option<&str>,
        auth: Option<&str>,
    ) -> (StatusCode, String, HeaderMap) {
        use axum::body::Body;
        use tower::ServiceExt;
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(k) = idem {
            builder = builder.header(HEADER_NAME, k);
        }
        if let Some(token) = auth {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let req = builder.body(Body::empty()).expect("request builder");
        let resp = router.clone().oneshot(req).await.expect("router responds");
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = to_bytes(resp.into_body(), 1 << 16)
            .await
            .expect("body collects")
            .to_vec();
        (status, String::from_utf8(body).unwrap_or_default(), headers)
    }

    #[tokio::test]
    async fn replay_returns_cached_response_without_re_running_handler() {
        let store = IdempotencyStore::with_defaults();
        let (mut router, counter) = counting_router(store);

        let (s1, b1, h1) =
            run(&mut router, Method::POST, "/mutate", Some("abc"), None).await;
        let (s2, b2, h2) =
            run(&mut router, Method::POST, "/mutate", Some("abc"), None).await;

        assert_eq!(counter.load(Ordering::SeqCst), 1, "handler ran once");
        assert_eq!(s1, s2);
        assert_eq!(b1, b2);
        assert!(
            h1.get("x-idempotent-replay").is_none(),
            "first call is not a replay",
        );
        assert_eq!(
            h2.get("x-idempotent-replay").and_then(|v| v.to_str().ok()),
            Some("true"),
            "second call is flagged as replay",
        );
    }

    #[tokio::test]
    async fn different_key_re_runs_handler() {
        let store = IdempotencyStore::with_defaults();
        let (mut router, counter) = counting_router(store);

        let (_, b1, _) =
            run(&mut router, Method::POST, "/mutate", Some("abc"), None).await;
        let (_, b2, _) =
            run(&mut router, Method::POST, "/mutate", Some("def"), None).await;

        assert_eq!(counter.load(Ordering::SeqCst), 2);
        assert_ne!(b1, b2);
    }

    #[tokio::test]
    async fn missing_header_passes_through() {
        let store = IdempotencyStore::with_defaults();
        let (mut router, counter) = counting_router(store.clone());

        let _ = run(&mut router, Method::POST, "/mutate", None, None).await;
        let _ = run(&mut router, Method::POST, "/mutate", None, None).await;

        assert_eq!(counter.load(Ordering::SeqCst), 2);
        assert_eq!(store.len(), 0, "cache untouched without header");
    }

    #[tokio::test]
    async fn get_method_passes_through_even_with_header() {
        let store = IdempotencyStore::with_defaults();
        let (mut router, counter) = counting_router(store.clone());

        let _ = run(&mut router, Method::GET, "/read", Some("abc"), None).await;
        let _ = run(&mut router, Method::GET, "/read", Some("abc"), None).await;

        assert_eq!(counter.load(Ordering::SeqCst), 0, "GET handler not counted");
        assert_eq!(store.len(), 0, "GET requests never cache");
    }

    #[tokio::test]
    async fn actor_namespace_isolates_keys() {
        let store = IdempotencyStore::with_defaults();
        let (mut router, counter) = counting_router(store);

        let (_, b1, _) = run(
            &mut router,
            Method::POST,
            "/mutate",
            Some("shared"),
            Some("alpha-token"),
        )
        .await;
        let (_, b2, _) = run(
            &mut router,
            Method::POST,
            "/mutate",
            Some("shared"),
            Some("beta-token"),
        )
        .await;

        assert_eq!(
            counter.load(Ordering::SeqCst),
            2,
            "different actors must not share the cache slot",
        );
        assert_ne!(b1, b2);
    }

    #[tokio::test]
    async fn expired_entries_are_evicted_on_access() {
        let store = IdempotencyStore::new(Duration::from_millis(50), 16);
        let (mut router, counter) = counting_router(store.clone());

        let (_, b1, _) =
            run(&mut router, Method::POST, "/mutate", Some("abc"), None).await;
        tokio::time::sleep(Duration::from_millis(80)).await;
        let (_, b2, _) =
            run(&mut router, Method::POST, "/mutate", Some("abc"), None).await;

        assert_eq!(counter.load(Ordering::SeqCst), 2, "TTL expiry re-runs handler");
        assert_ne!(b1, b2);
    }
}
