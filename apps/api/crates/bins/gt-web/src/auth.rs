//! Bearer-token IAM at the HTTP boundary (doc 07). The router applies the middleware once at
//! the gateway and the domain handlers stay free of auth concerns. Three modes:
//!
//! - [`AuthConfig::Open`] — disabled, every request reaches the handler. Reserved for in-
//!   process tests; the binary fails to start without an explicit token.
//! - [`AuthConfig::Bearer`] — requires `Authorization: Bearer <token>`; the token is compared
//!   with constant-time equality against the env-loaded shared secret. The actor identity
//!   recorded in the audit log is derived from the bearer prefix so the secret never lands
//!   in the events.jsonl.
//! - [`AuthConfig::Jwt`] — HS256-signed token (hq-fe-rbac.1). The middleware verifies the
//!   signature + `exp` + `iss` and forwards the typed [`crate::jwt::Claims`] to handlers as
//!   an [`AuthClaims`] extension. `roles`/`scopes` ride the token and stay empty until
//!   hq-fe-rbac.2 starts populating them at issuance; the wire shape is already final so
//!   `/api/whoami` can return them today without breaking on the upgrade.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use sha2::{Digest, Sha256};

use crate::audit::{WebAuditEvent, WebAuditSink};
use crate::jwt::{Claims, JwtError, JwtIssuer};

/// Auth posture for the router. Open mode skips both the check and the `invoked`/`unauthorized`
/// audit records — useful only for the unit tests of the snapshot/SSE wiring that pre-date the
/// auth boundary.
#[derive(Clone)]
pub enum AuthConfig {
    Open,
    Bearer { secret: Arc<String> },
    /// HS256 JWT mode. The `Arc<JwtIssuer>` is shared by every request: encoding/decoding
    /// keys live inside the issuer, never on the per-request hot path.
    Jwt { issuer: Arc<JwtIssuer> },
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

    /// Build the JWT variant from a pre-constructed issuer. `gt-web` boot uses this with
    /// [`JwtIssuer::shared`] so the same `Arc` is also reused by a future `POST /api/login`
    /// route (hq-fe-auth) without rebuilding the keys.
    pub fn jwt(issuer: Arc<JwtIssuer>) -> Self {
        Self::Jwt { issuer }
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
/// open mode carries the literal `web:open` so the dev fall-through is observable;
/// JWT mode carries the verified `sub` claim verbatim.
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

/// Verified JWT body forwarded to handlers via request extensions. Present only when the
/// gateway runs in [`AuthConfig::Jwt`] mode; bearer/open requests have no extension and
/// handlers should treat absence as "no claims" rather than "denied". Cheap to clone —
/// the body is a small struct of `String` + `Vec<String>` fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthClaims(pub Claims);

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

/// Map a verifier error to the short reason string that lands in `web.unauthorized`.
/// Kept narrow on purpose: the audit log distinguishes failure modes, but the wire reply
/// stays opaque so a probing client cannot tell `expired` from `tampered`.
fn jwt_audit_reason(err: &JwtError) -> &'static str {
    match err {
        JwtError::Expired => "expired jwt",
        JwtError::InvalidSignature => "invalid jwt signature",
        JwtError::Malformed(_) => "malformed jwt",
        JwtError::WrongIssuer => "jwt issuer mismatch",
        // `NoRbac` is an issuance-side error (sign_for_actor without bound config); the
        // verifier path never produces it. Folded into the catch-all for completeness.
        JwtError::NoRbac | JwtError::Other(_) => "invalid jwt",
    }
}

/// Middleware applied to every routed request. Open mode is a passthrough; Bearer and JWT
/// modes enforce the header and emit one audit record per request (invoked or
/// unauthorized).
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
        AuthConfig::Jwt { issuer } => {
            let Some(presented) = extract_bearer(&req) else {
                layer.audit.record(WebAuditEvent::Unauthorized {
                    method: method.clone(),
                    path: path.clone(),
                    reason: "missing jwt".into(),
                });
                return unauthorized("missing bearer token");
            };
            let claims = match issuer.verify(presented) {
                Ok(c) => c,
                Err(e) => {
                    layer.audit.record(WebAuditEvent::Unauthorized {
                        method: method.clone(),
                        path: path.clone(),
                        reason: jwt_audit_reason(&e).into(),
                    });
                    return unauthorized("invalid bearer token");
                }
            };
            let actor = claims.sub.clone();
            req.extensions_mut().insert(Actor(actor.clone()));
            req.extensions_mut().insert(AuthClaims(claims));
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
    use std::time::Duration;

    use axum::body::to_bytes;
    use axum::http::{Method, Request, StatusCode};
    use axum::middleware::from_fn_with_state;
    use axum::routing::get;
    use axum::Router;
    use tower::ServiceExt;

    use crate::audit::InMemoryWebAudit;

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

    /// Probe handler that echoes the actor + claims into the response body so the
    /// middleware tests can assert extensions land on the request without spinning up the
    /// full router.
    async fn probe(
        actor: axum::extract::Extension<Actor>,
        claims: Option<axum::extract::Extension<AuthClaims>>,
    ) -> String {
        let actor = actor.0 .0;
        let claims = match claims {
            Some(c) => format!(
                "sub={};roles={};scopes={}",
                c.0 .0.sub,
                c.0 .0.roles.join(","),
                c.0 .0.scopes.join(",")
            ),
            None => "no-claims".into(),
        };
        format!("actor={actor}|{claims}")
    }

    fn router_with(layer: AuthLayer) -> Router {
        Router::new()
            .route("/probe", get(probe))
            .layer(from_fn_with_state(layer, auth_middleware))
    }

    async fn body_text(resp: axum::response::Response) -> String {
        let bytes = to_bytes(resp.into_body(), 1024).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn jwt_mode_accepts_signed_token_and_forwards_claims() {
        let issuer = JwtIssuer::from_secret("sec").shared();
        let audit = Arc::new(InMemoryWebAudit::new());
        let layer = AuthLayer {
            config: AuthConfig::jwt(issuer.clone()),
            audit: audit.clone(),
        };
        let app = router_with(layer);
        let token = issuer
            .sign(
                "claude-host",
                vec!["sheriff".into()],
                vec!["beads.write".into()],
            )
            .unwrap();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/probe")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_text(resp).await;
        assert!(
            body.contains("actor=claude-host"),
            "expected sub-derived actor, got {body}"
        );
        assert!(body.contains("sub=claude-host"));
        assert!(body.contains("roles=sheriff"));
        assert!(body.contains("scopes=beads.write"));

        let invoked = audit.snapshot();
        assert_eq!(invoked.len(), 1, "exactly one audit record per request");
        match &invoked[0] {
            WebAuditEvent::Invoked { actor, status, .. } => {
                assert_eq!(actor, "claude-host");
                assert_eq!(*status, 200);
            }
            other => panic!("expected Invoked, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn jwt_mode_rejects_missing_token() {
        let issuer = JwtIssuer::from_secret("sec").shared();
        let audit = Arc::new(InMemoryWebAudit::new());
        let app = router_with(AuthLayer {
            config: AuthConfig::jwt(issuer),
            audit: audit.clone(),
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/probe")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        match audit.snapshot().first().unwrap() {
            WebAuditEvent::Unauthorized { reason, .. } => assert_eq!(reason, "missing jwt"),
            other => panic!("expected Unauthorized, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn jwt_mode_rejects_expired_token_with_distinct_audit_reason() {
        let issuer = JwtIssuer::with_ttl("sec", Duration::from_secs(1)).shared();
        let audit = Arc::new(InMemoryWebAudit::new());
        let app = router_with(AuthLayer {
            config: AuthConfig::jwt(issuer.clone()),
            audit: audit.clone(),
        });
        let token = issuer.sign("a", vec![], vec![]).unwrap();
        std::thread::sleep(Duration::from_secs(2));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/probe")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        match audit.snapshot().first().unwrap() {
            WebAuditEvent::Unauthorized { reason, .. } => assert_eq!(reason, "expired jwt"),
            other => panic!("expected Unauthorized, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn jwt_mode_rejects_token_signed_with_wrong_secret() {
        let server = JwtIssuer::from_secret("server-key").shared();
        let attacker = JwtIssuer::from_secret("other-key");
        let token = attacker.sign("evil", vec![], vec![]).unwrap();
        let audit = Arc::new(InMemoryWebAudit::new());
        let app = router_with(AuthLayer {
            config: AuthConfig::jwt(server),
            audit: audit.clone(),
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/probe")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        match audit.snapshot().first().unwrap() {
            WebAuditEvent::Unauthorized { reason, .. } => {
                assert_eq!(reason, "invalid jwt signature")
            }
            other => panic!("expected Unauthorized, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn bearer_mode_does_not_attach_auth_claims() {
        let audit = Arc::new(InMemoryWebAudit::new());
        let app = router_with(AuthLayer {
            config: AuthConfig::bearer("tok"),
            audit,
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/probe")
                    .header(header::AUTHORIZATION, "Bearer tok")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_text(resp).await;
        assert!(
            body.contains("no-claims"),
            "bearer mode should not set AuthClaims, got {body}"
        );
    }
}
