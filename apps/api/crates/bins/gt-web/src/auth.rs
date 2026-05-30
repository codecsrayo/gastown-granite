//! Bearer-token IAM at the HTTP boundary (doc 07). The router applies the middleware once at
//! the gateway and the domain handlers stay free of auth concerns. Two modes:
//!
//! - [`AuthConfig::Open`] — disabled, every request reaches the handler. Reserved for in-
//!   process tests; the binary fails to start without an explicit token.
//! - [`AuthConfig::Bearer`] — requires `Authorization: Bearer <token>`; the token is compared
//!   with constant-time equality against the env-loaded shared secret. The actor identity
//!   recorded in the audit log is derived from the bearer prefix so the secret never lands
//!   in the events.jsonl.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use sha2::{Digest, Sha256};

use crate::audit::{WebAuditEvent, WebAuditSink};

/// Auth posture for the router. Open mode skips both the check and the `invoked`/`unauthorized`
/// audit records — useful only for the unit tests of the snapshot/SSE wiring that pre-date the
/// auth boundary.
#[derive(Clone)]
pub enum AuthConfig {
    Open,
    Bearer { secret: Arc<String> },
}

impl AuthConfig {
    pub fn open() -> Self {
        Self::Open
    }

    pub fn bearer(secret: impl Into<String>) -> Self {
        Self::Bearer {
            secret: Arc::new(secret.into()),
        }
    }
}

/// Shared dependencies the auth middleware needs. Carried as router state so axum can clone it
/// per request without locking.
#[derive(Clone)]
pub struct AuthLayer {
    pub config: AuthConfig,
    pub audit: Arc<dyn WebAuditSink>,
}

/// Identity tag attached to a successfully-authorized request. 12-char prefix of the
/// SHA-256 of the bearer token: stable across requests, not the raw secret.
pub fn actor_tag(secret: &str) -> String {
    let mut h = Sha256::new();
    h.update(secret.as_bytes());
    let digest = h.finalize();
    let hex: String = digest.iter().map(|b| format!("{:02x}", b)).collect();
    format!("web:{}", &hex[..12])
}

/// Identity propagated into request extensions by [`auth_middleware`] so downstream
/// handlers (e.g. `GET /api/whoami` per hq-fe-rbac.4) can read who they're answering
/// without re-extracting the bearer header. Bearer mode carries the [`actor_tag`];
/// open mode carries the literal `web:open` so the dev fall-through is observable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Actor(pub String);

impl Actor {
    pub fn open() -> Self {
        Self("web:open".to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut acc: u8 = 0;
    for (x, y) in a.bytes().zip(b.bytes()) {
        acc |= x ^ y;
    }
    acc == 0
}

fn extract_bearer(req: &Request<Body>) -> Option<&str> {
    let header = req.headers().get(header::AUTHORIZATION)?;
    let value = header.to_str().ok()?;
    value.strip_prefix("Bearer ").map(str::trim)
}

fn unauthorized(reason: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({ "error": reason })),
    )
        .into_response()
}

/// Middleware applied to every routed request. Open mode is a passthrough; Bearer mode
/// enforces the header and emits one audit record per request (invoked or unauthorized).
pub async fn auth_middleware(
    State(layer): State<AuthLayer>,
    mut req: Request<Body>,
    next: Next,
) -> Response {
    let method = req.method().to_string();
    let path = req.uri().path().to_string();

    match &layer.config {
        AuthConfig::Open => {
            req.extensions_mut().insert(Actor::open());
            next.run(req).await
        }
        AuthConfig::Bearer { secret } => {
            let Some(presented) = extract_bearer(&req) else {
                layer.audit.record(WebAuditEvent::Unauthorized {
                    method: method.clone(),
                    path: path.clone(),
                    reason: "missing bearer token".into(),
                });
                return unauthorized("missing bearer token");
            };
            if !constant_time_eq(presented, secret.as_str()) {
                layer.audit.record(WebAuditEvent::Unauthorized {
                    method: method.clone(),
                    path: path.clone(),
                    reason: "invalid bearer token".into(),
                });
                return unauthorized("invalid bearer token");
            }
            let actor = actor_tag(secret.as_str());
            req.extensions_mut().insert(Actor(actor.clone()));
            let resp = next.run(req).await;
            layer.audit.record(WebAuditEvent::Invoked {
                actor,
                method,
                path,
                status: resp.status().as_u16(),
            });
            resp
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_matches_only_on_equal_strings() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "abcd"));
    }

    #[test]
    fn actor_tag_is_deterministic_and_redacts_secret() {
        let tag_a = actor_tag("topsecret");
        let tag_b = actor_tag("topsecret");
        assert_eq!(tag_a, tag_b);
        assert!(tag_a.starts_with("web:"));
        assert!(!tag_a.contains("topsecret"));
        assert_ne!(actor_tag("topsecret"), actor_tag("other"));
    }
}
