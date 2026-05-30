//! HS256 JWT issuer + verifier for the `gt-web` gateway (hq-fe-rbac.1).
//!
//! Why HS256 and not RS256:
//! - `gt-api` is a single binary that both issues and verifies the token. There is no
//!   external service in the trust boundary that needs a public key, so a symmetric secret
//!   is sufficient and avoids JWKS / key-rotation ceremony.
//! - The existing IAM frontier already shares a secret via `GT_WEB_TOKEN`; switching to
//!   `GT_WEB_JWT_SECRET` is a one-env-var migration with the same operational surface.
//! - When (and only when) a separate verifier joins the boundary — `gt-mcp` over HTTP,
//!   an external IDP, multi-tenant deploys — switch the `Algorithm` to `RS256` and load
//!   PEM keys from disk. The `Claims` shape stays unchanged so the FE contract holds.
//!
//! What this module is:
//! - [`Claims`] — the canonical JWT body the gateway issues and the middleware verifies.
//!   `sub` is the actor id (e.g. `web:claude-host`), `roles` + `scopes` mirror the RBAC
//!   contract `/api/whoami` returns. Empty until [hq-fe-rbac.2] populates them from
//!   `roles.toml`.
//! - [`JwtIssuer`] — composes encoding + decoding keys (same secret) + a default TTL.
//!   `sign` mints a token for an actor + role/scope set; `verify` returns the claims when
//!   the signature is valid and `exp` has not passed. Both `iat` and `exp` are i64
//!   Unix-second timestamps to stay JSON-friendly with negative offsets.
//!
//! What this module is **not**:
//! - A login endpoint. Issuance is a library call today; the HTTP route lands in the
//!   `hq-fe-auth` epic (pty driver against Claude's `/login`).
//! - A scope checker. Per-route enforcement is `hq-fe-rbac.3`. Today the middleware just
//!   verifies the signature and forwards the claims to handlers via `AuthClaims`.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jsonwebtoken::{
    decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation,
};
use serde::{Deserialize, Serialize};

/// Default token lifetime when the issuer is built without an explicit TTL. Sized to a
/// dashboard session: short enough that a revoked actor stops working within an hour,
/// long enough that an operator does not re-auth between coffee breaks.
pub const DEFAULT_TTL: Duration = Duration::from_secs(60 * 60);

/// Canonical issuer claim. Lets a future verifier (gt-mcp over HTTP, external IDP) reject
/// tokens minted by a different gateway. Bound to the binary name on purpose so a copy of
/// the same secret in another component does not silently inherit trust.
pub const ISSUER: &str = "gt-web";

/// JWT body. Field order matches the [RFC 7519] registered claim ordering for the standard
/// fields; the RBAC additions (`roles`, `scopes`) sit at the end so the encoded payload
/// stays diff-friendly across versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claims {
    /// Subject — the actor id the gateway propagates as [`crate::auth::Actor`]. For the
    /// current bearer-style fallback this is `web:<short-hash>`; once a real login flow
    /// lands the subject is the account name (`claude-host`, `polecat-7`, ...).
    pub sub: String,
    /// Issuer — always [`ISSUER`] today; rejected on verify if it ever drifts.
    pub iss: String,
    /// Issued-at, seconds since Unix epoch.
    pub iat: i64,
    /// Expiry, seconds since Unix epoch. Tokens past their `exp` are rejected by
    /// `jsonwebtoken::Validation` with leeway = 0.
    pub exp: i64,
    /// Role assignments. Empty until `hq-fe-rbac.2` lands the `roles.toml` loader.
    #[serde(default)]
    pub roles: Vec<String>,
    /// Scope assignments. Empty until `hq-fe-rbac.2`; per-route enforcement is
    /// `hq-fe-rbac.3`.
    #[serde(default)]
    pub scopes: Vec<String>,
}

/// Issuer + verifier built around a single HS256 secret. Held by `AuthConfig::Jwt` and
/// cloned per request via the surrounding `Arc`; both keys are owned `Vec<u8>` copies of
/// the same secret bytes so cloning is cheap (`Arc` does the sharing).
#[derive(Clone)]
pub struct JwtIssuer {
    encoding: EncodingKey,
    decoding: DecodingKey,
    validation: Validation,
    ttl: Duration,
}

/// Failure modes the verifier surfaces. `Expired` is split out so the audit log can
/// distinguish "stale session" from "tampered token" without parsing the upstream error
/// string. Encoding failures fall through to `Other` because they are unreachable under
/// normal use (HS256 + valid secret = always encodes).
#[derive(Debug, thiserror::Error)]
pub enum JwtError {
    #[error("token expired")]
    Expired,
    #[error("invalid signature")]
    InvalidSignature,
    #[error("malformed token: {0}")]
    Malformed(String),
    #[error("issuer mismatch")]
    WrongIssuer,
    #[error("jwt: {0}")]
    Other(String),
}

impl JwtIssuer {
    /// Build an issuer from a raw secret. The secret must be non-empty; the caller is
    /// responsible for sourcing it from a sealed channel (env var today, secret manager
    /// later). Uses [`DEFAULT_TTL`].
    pub fn from_secret(secret: impl AsRef<[u8]>) -> Self {
        Self::with_ttl(secret, DEFAULT_TTL)
    }

    /// Build an issuer with an explicit token lifetime. Tests pin a tight TTL so the
    /// expiry path runs without sleeping; production keeps [`DEFAULT_TTL`].
    pub fn with_ttl(secret: impl AsRef<[u8]>, ttl: Duration) -> Self {
        let bytes = secret.as_ref();
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_issuer(&[ISSUER]);
        validation.leeway = 0;
        Self {
            encoding: EncodingKey::from_secret(bytes),
            decoding: DecodingKey::from_secret(bytes),
            validation,
            ttl,
        }
    }

    /// Wrap the issuer in an `Arc`. Convenience for handing it to `AuthConfig::Jwt` —
    /// every request clones the inner `Arc`, never the keys.
    pub fn shared(self) -> Arc<Self> {
        Arc::new(self)
    }

    /// Mint a token for an actor. `roles` and `scopes` are stamped verbatim; the issuer
    /// does not consult `roles.toml` (that is the caller's job once it exists).
    pub fn sign(
        &self,
        subject: impl Into<String>,
        roles: Vec<String>,
        scopes: Vec<String>,
    ) -> Result<String, JwtError> {
        let now = unix_now();
        let claims = Claims {
            sub: subject.into(),
            iss: ISSUER.to_string(),
            iat: now,
            exp: now + self.ttl.as_secs() as i64,
            roles,
            scopes,
        };
        encode(&Header::new(Algorithm::HS256), &claims, &self.encoding)
            .map_err(|e| JwtError::Other(e.to_string()))
    }

    /// Verify a token against the issuer's secret. Returns the typed claims on success
    /// so the middleware can forward them to handlers without re-parsing.
    pub fn verify(&self, token: &str) -> Result<Claims, JwtError> {
        match decode::<Claims>(token, &self.decoding, &self.validation) {
            Ok(data) => Ok(data.claims),
            Err(e) => Err(map_jwt_error(e)),
        }
    }

    /// Exposed for tests + the dev `sign` CLI helper (hq-fe-auth).
    pub fn ttl(&self) -> Duration {
        self.ttl
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn map_jwt_error(e: jsonwebtoken::errors::Error) -> JwtError {
    use jsonwebtoken::errors::ErrorKind;
    match e.kind() {
        ErrorKind::ExpiredSignature => JwtError::Expired,
        ErrorKind::InvalidSignature => JwtError::InvalidSignature,
        ErrorKind::InvalidIssuer => JwtError::WrongIssuer,
        ErrorKind::Base64(_)
        | ErrorKind::Json(_)
        | ErrorKind::Utf8(_)
        | ErrorKind::InvalidToken => JwtError::Malformed(e.to_string()),
        _ => JwtError::Other(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn sign_then_verify_roundtrips() {
        let issuer = JwtIssuer::from_secret("test-secret");
        let token = issuer
            .sign(
                "claude-host",
                vec!["sheriff".into()],
                vec!["beads.write".into()],
            )
            .unwrap();
        let claims = issuer.verify(&token).unwrap();
        assert_eq!(claims.sub, "claude-host");
        assert_eq!(claims.iss, ISSUER);
        assert_eq!(claims.roles, vec!["sheriff".to_string()]);
        assert_eq!(claims.scopes, vec!["beads.write".to_string()]);
        assert!(claims.exp > claims.iat);
    }

    #[test]
    fn wrong_secret_rejects() {
        let signer = JwtIssuer::from_secret("alpha");
        let token = signer.sign("a", vec![], vec![]).unwrap();
        let other = JwtIssuer::from_secret("beta");
        let err = other.verify(&token).unwrap_err();
        assert!(matches!(err, JwtError::InvalidSignature), "got {err:?}");
    }

    #[test]
    fn expired_token_rejects() {
        let issuer = JwtIssuer::with_ttl("k", Duration::from_secs(1));
        let token = issuer.sign("a", vec![], vec![]).unwrap();
        sleep(Duration::from_secs(2));
        let err = issuer.verify(&token).unwrap_err();
        assert!(matches!(err, JwtError::Expired), "got {err:?}");
    }

    #[test]
    fn malformed_token_rejects() {
        let issuer = JwtIssuer::from_secret("k");
        let err = issuer.verify("not-a-jwt").unwrap_err();
        assert!(matches!(err, JwtError::Malformed(_)), "got {err:?}");
    }

    #[test]
    fn issuer_mismatch_rejects() {
        // Sign with a hand-rolled `iss` field by going around the issuer's helper.
        let issuer = JwtIssuer::from_secret("k");
        let now = unix_now();
        let bad = Claims {
            sub: "a".into(),
            iss: "someone-else".into(),
            iat: now,
            exp: now + 60,
            roles: vec![],
            scopes: vec![],
        };
        let token = encode(
            &Header::new(Algorithm::HS256),
            &bad,
            &EncodingKey::from_secret(b"k"),
        )
        .unwrap();
        let err = issuer.verify(&token).unwrap_err();
        assert!(matches!(err, JwtError::WrongIssuer), "got {err:?}");
    }
}
