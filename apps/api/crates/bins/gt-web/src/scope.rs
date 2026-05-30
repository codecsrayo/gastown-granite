//! Per-route scope enforcement (hq-fe-rbac.3). Replaces the single-bearer "you're in or
//! you're out" posture with a per-handler capability check that consults the verified
//! [`crate::auth::AuthClaims`] forwarded by the JWT middleware.
//!
//! Layering model:
//!
//! - [`crate::auth::auth_middleware`] runs first and stamps the request with [`Actor`] and
//!   (in JWT mode) [`AuthClaims`]. The router applies it once at the gateway.
//! - [`scope_middleware`] runs as a per-route layer. It inspects the claims and either
//!   forwards the request to the handler or short-circuits with `403 Forbidden`.
//!
//! Posture-aware grandfather rule: only **JWT mode** carries claims, so only JWT mode is
//! gated. [`AuthConfig::Bearer`] (single shared secret) and [`AuthConfig::Open`] (tests)
//! pre-date the RBAC contract and grant full access by design — a deploy that still uses
//! `GT_WEB_TOKEN` keeps working unchanged. The middleware detects this by the absence of
//! [`AuthClaims`] in the request extensions; once `GT_WEB_JWT_SECRET` is set, every route
//! starts enforcing its declared scope.
//!
//! Audit shape: a rejection emits one [`WebAuditEvent::Forbidden`] with the actor tag,
//! method/path, and the missing scope so the operator can tell from the event log which
//! grant needs widening. Accept paths leave the existing `web.invoked` record untouched —
//! the gateway already records it from `auth_middleware`.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::audit::{WebAuditEvent, WebAuditSink};
use crate::auth::{Actor, AuthClaims};

/// State carried by [`scope_middleware`]. Built once per route via [`require_scope`] so the
/// captured `scope` is a `&'static str` and no per-request allocation happens on the hot
/// path. The audit sink is the same `Arc` the gateway-wide [`crate::auth::AuthLayer`] holds.
#[derive(Clone)]
pub struct ScopeGuard {
    pub audit: Arc<dyn WebAuditSink>,
    pub scope: &'static str,
}

/// Middleware body. Reads [`AuthClaims`] from the request extensions; if present, the
/// declared `scope` must appear in `claims.scopes` or the request is rejected with 403.
/// Absence of claims means the gateway is in Bearer/Open mode — see the module doc for
/// the grandfather rationale.
///
/// Production wiring lives in `lib.rs::router_with_stores`, which attaches one
/// `from_fn_with_state(ScopeGuard { audit, scope: "..." }, scope_middleware)` per route
/// via `Router::route_layer`.
pub async fn scope_middleware(
    State(guard): State<ScopeGuard>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let claims = req.extensions().get::<AuthClaims>().cloned();
    let Some(claims) = claims else {
        return next.run(req).await;
    };
    if claims.0.scopes.iter().any(|s| s == guard.scope) {
        return next.run(req).await;
    }
    let actor = req
        .extensions()
        .get::<Actor>()
        .map(|a| a.0.clone())
        .unwrap_or_default();
    let method = req.method().to_string();
    let path = req.uri().path().to_string();
    guard.audit.record(WebAuditEvent::Forbidden {
        actor,
        method,
        path,
        scope: guard.scope.to_string(),
    });
    forbidden(guard.scope)
}

fn forbidden(scope: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({
            "error": "missing required scope",
            "scope": scope,
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::{header, Method, Request};
    use axum::middleware::from_fn_with_state;
    use axum::routing::get;
    use axum::Router;
    use tower::ServiceExt;

    use crate::audit::InMemoryWebAudit;
    use crate::auth::{auth_middleware, AuthConfig, AuthLayer};
    use crate::jwt::JwtIssuer;

    async fn ok() -> &'static str {
        "ok"
    }

    fn router(layer: AuthLayer, guard: ScopeGuard) -> Router {
        Router::new()
            .route(
                "/probe",
                get(ok).route_layer(from_fn_with_state(guard, scope_middleware)),
            )
            .layer(from_fn_with_state(layer, auth_middleware))
    }

    async fn body_text(resp: Response) -> String {
        let bytes = to_bytes(resp.into_body(), 1024).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn jwt_mode_with_matching_scope_passes() {
        let issuer = JwtIssuer::from_secret("sec").shared();
        let audit = Arc::new(InMemoryWebAudit::new());
        let layer = AuthLayer {
            config: AuthConfig::jwt(issuer.clone()),
            audit: audit.clone(),
        };
        let guard = ScopeGuard {
            audit: audit.clone(),
            scope: "beads.write",
        };
        let app = router(layer, guard);
        let token = issuer
            .sign(
                "claude-host",
                vec!["sheriff".into()],
                vec!["beads.write".into(), "merge.read".into()],
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
        assert_eq!(body_text(resp).await, "ok");
        // The auth middleware still records its accept; no Forbidden lands.
        assert!(audit
            .snapshot()
            .iter()
            .all(|e| !matches!(e, WebAuditEvent::Forbidden { .. })));
    }

    #[tokio::test]
    async fn jwt_mode_missing_scope_is_forbidden_and_audited() {
        let issuer = JwtIssuer::from_secret("sec").shared();
        let audit = Arc::new(InMemoryWebAudit::new());
        let layer = AuthLayer {
            config: AuthConfig::jwt(issuer.clone()),
            audit: audit.clone(),
        };
        let guard = ScopeGuard {
            audit: audit.clone(),
            scope: "beads.write",
        };
        let app = router(layer, guard);
        // Token carries only `merge.read`, not `beads.write`.
        let token = issuer
            .sign("watcher", vec!["reader".into()], vec!["merge.read".into()])
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
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let body = body_text(resp).await;
        assert!(body.contains("missing required scope"));
        assert!(body.contains("beads.write"));
        let forbidden = audit
            .snapshot()
            .into_iter()
            .find_map(|e| match e {
                WebAuditEvent::Forbidden {
                    actor,
                    method,
                    path,
                    scope,
                } => Some((actor, method, path, scope)),
                _ => None,
            })
            .expect("Forbidden audit record expected");
        assert_eq!(forbidden.0, "watcher");
        assert_eq!(forbidden.1, "GET");
        assert_eq!(forbidden.2, "/probe");
        assert_eq!(forbidden.3, "beads.write");
    }

    #[tokio::test]
    async fn bearer_mode_grandfathers_through_without_claims() {
        let audit = Arc::new(InMemoryWebAudit::new());
        let layer = AuthLayer {
            config: AuthConfig::bearer("tok"),
            audit: audit.clone(),
        };
        let guard = ScopeGuard {
            audit: audit.clone(),
            scope: "beads.write",
        };
        let app = router(layer, guard);
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
        // No Forbidden audit — bearer mode predates per-scope RBAC.
        assert!(audit
            .snapshot()
            .iter()
            .all(|e| !matches!(e, WebAuditEvent::Forbidden { .. })));
    }

    #[tokio::test]
    async fn open_mode_grandfathers_through_without_claims() {
        let audit = Arc::new(InMemoryWebAudit::new());
        let layer = AuthLayer {
            config: AuthConfig::open(),
            audit: audit.clone(),
        };
        let guard = ScopeGuard {
            audit: audit.clone(),
            scope: "beads.write",
        };
        let app = router(layer, guard);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/probe")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn empty_scope_set_on_token_is_denied() {
        let issuer = JwtIssuer::from_secret("sec").shared();
        let audit = Arc::new(InMemoryWebAudit::new());
        let layer = AuthLayer {
            config: AuthConfig::jwt(issuer.clone()),
            audit: audit.clone(),
        };
        let guard = ScopeGuard {
            audit: audit.clone(),
            scope: "beads.write",
        };
        let app = router(layer, guard);
        let token = issuer.sign("stranger", vec![], vec![]).unwrap();
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
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }
}
