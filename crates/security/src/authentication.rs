//! The [`Authentication`] context type, the [`Verifier`] port, and the
//! request-extension helpers that mirror the Go port's
//! `context.Context` accessors.

use std::collections::HashMap;
use std::future::Future;

use async_trait::async_trait;
use http::Request;
use serde::{Deserialize, Serialize};

/// `Authentication` is the principal + authorities tuple stored on the
/// request after successful auth.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Authentication {
    /// Unique stable id (`sub` claim).
    pub principal: String,
    /// Human-friendly name.
    pub username: String,
    /// Authorities; `"ROLE_XYZ"` or domain-specific strings.
    pub roles: Vec<String>,
    /// Fine-grained authorities (pyfly: `permissions`) — e.g. OAuth2
    /// scopes or explicit permission strings, distinct from [`roles`].
    /// Populated by [`JwksVerifier`](crate::JwksVerifier) from the
    /// `permissions` claim or the space-separated `scope` claim.
    ///
    /// [`roles`]: Authentication::roles
    pub authorities: Vec<String>,
    /// Raw token claims, available to handlers.
    pub claims: HashMap<String, serde_json::Value>,
}

impl Authentication {
    /// Reports whether the authentication carries `role`.
    pub fn has_role(&self, role: &str) -> bool {
        self.roles.iter().any(|r| r == role)
    }

    /// Returns true if any role matches.
    pub fn has_any_role(&self, roles: &[&str]) -> bool {
        roles.iter().any(|want| self.has_role(want))
    }

    /// Reports whether the authentication carries `authority` — true
    /// when it appears in [`authorities`](Authentication::authorities)
    /// **or** [`roles`](Authentication::roles) (pyfly's `hasAuthority`
    /// accepts a role name or a permission).
    pub fn has_authority(&self, authority: &str) -> bool {
        self.authorities.iter().any(|a| a == authority) || self.has_role(authority)
    }

    /// Returns true if any authority matches (see
    /// [`has_authority`](Authentication::has_authority)).
    pub fn has_any_authority(&self, authorities: &[&str]) -> bool {
        authorities.iter().any(|want| self.has_authority(want))
    }

    /// Returns the anonymous authentication — principal [`ANONYMOUS_ID`],
    /// no username, no roles, no claims.
    pub fn anonymous() -> Self {
        Self {
            principal: ANONYMOUS_ID.to_owned(),
            ..Self::default()
        }
    }
}

/// `ANONYMOUS_ID` is the principal id used when no auth is present and
/// [`BearerLayer`](crate::BearerLayer) is configured to allow anonymous
/// access.
pub const ANONYMOUS_ID: &str = "anonymous";

/// `SecurityError` is the typed error family of the security tier.
///
/// The `Display` strings match the Go port's sentinel errors exactly,
/// so problem `detail` members are identical across runtimes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SecurityError {
    /// Returned by guards when no valid auth is present
    /// (Go: `ErrUnauthenticated`).
    #[error("firefly/security: unauthenticated")]
    Unauthenticated,
    /// Returned by guards when auth is present but authorities don't
    /// match (Go: `ErrForbidden`).
    #[error("firefly/security: forbidden")]
    Forbidden,
    /// The configured header is present but not `Bearer <token>` shaped.
    #[error("malformed Authorization header")]
    MalformedHeader,
    /// Token verification failed; the message comes verbatim from the
    /// [`Verifier`] and becomes the problem `detail`.
    #[error("{0}")]
    Verification(String),
}

impl SecurityError {
    /// Builds a [`SecurityError::Verification`] from any message —
    /// the Rust analog of a Go verifier returning an ad-hoc `error`.
    pub fn verification(msg: impl Into<String>) -> Self {
        Self::Verification(msg.into())
    }
}

/// `Verifier` is the authentication port. Implementations validate the
/// raw token and return the resolved [`Authentication`]. Concrete
/// implementations satisfy this trait from the IDP crates.
#[async_trait]
pub trait Verifier: Send + Sync {
    /// Validates `token` and resolves the authenticated principal.
    async fn verify(&self, token: &str) -> Result<Authentication, SecurityError>;
}

/// `VerifierFn` adapts a plain async function to the [`Verifier`] trait
/// — the Rust analog of Go's `VerifierFunc`.
///
/// ```rust
/// use firefly_security::{Authentication, SecurityError, VerifierFn};
///
/// let v = VerifierFn(|token: String| async move {
///     if token == "good" {
///         Ok(Authentication::default())
///     } else {
///         Err(SecurityError::verification("nope"))
///     }
/// });
/// ```
pub struct VerifierFn<F>(pub F);

#[async_trait]
impl<F, Fut> Verifier for VerifierFn<F>
where
    F: Fn(String) -> Fut + Send + Sync,
    Fut: Future<Output = Result<Authentication, SecurityError>> + Send,
{
    async fn verify(&self, token: &str) -> Result<Authentication, SecurityError> {
        (self.0)(token.to_owned()).await
    }
}

/// Returns the request with `auth` attached — the Rust analog of Go's
/// `WithAuthentication(ctx, auth)`. [`BearerLayer`](crate::BearerLayer)
/// calls this for you; tests and custom middleware can use it directly.
pub fn with_authentication<B>(mut req: Request<B>, auth: Authentication) -> Request<B> {
    req.extensions_mut().insert(auth);
    req
}

/// Retrieves the authentication from the request, if present — the
/// Rust analog of Go's `AuthenticationFrom(ctx)`.
pub fn authentication_from<B>(req: &Request<B>) -> Option<&Authentication> {
    req.extensions().get::<Authentication>()
}

/// Returns the authentication or panics — handy in handlers that are
/// only reachable behind [`BearerLayer`](crate::BearerLayer)
/// (Go: `MustAuthFrom`).
///
/// # Panics
///
/// Panics if no [`Authentication`] is attached to the request.
pub fn must_auth_from<B>(req: &Request<B>) -> &Authentication {
    authentication_from(req).expect("firefly/security: no authentication on context")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth(roles: &[&str]) -> Authentication {
        Authentication {
            principal: "u1".into(),
            username: "alice".into(),
            roles: roles.iter().map(|r| r.to_string()).collect(),
            authorities: Vec::new(),
            claims: HashMap::new(),
        }
    }

    #[test]
    fn has_role_matches_exact() {
        let a = auth(&["USER", "ADMIN"]);
        assert!(a.has_role("USER"));
        assert!(a.has_role("ADMIN"));
        assert!(!a.has_role("OPERATOR"));
        assert!(!a.has_role("user")); // exact, case-sensitive — as in Go
    }

    #[test]
    fn has_any_role_matches_any() {
        let a = auth(&["USER"]);
        assert!(a.has_any_role(&["ADMIN", "USER"]));
        assert!(!a.has_any_role(&["ADMIN", "OPERATOR"]));
        assert!(!a.has_any_role(&[]));
    }

    #[test]
    fn anonymous_has_anonymous_principal_and_nothing_else() {
        let a = Authentication::anonymous();
        assert_eq!(a.principal, ANONYMOUS_ID);
        assert!(a.username.is_empty());
        assert!(a.roles.is_empty());
        assert!(a.authorities.is_empty());
        assert!(a.claims.is_empty());
    }

    #[test]
    fn has_authority_matches_authorities_and_roles() {
        let mut a = auth(&["ADMIN"]);
        a.authorities = vec!["read".into(), "write".into()];
        assert!(a.has_authority("read"));
        assert!(a.has_authority("write"));
        assert!(a.has_authority("ADMIN")); // role names count as authorities
        assert!(!a.has_authority("delete"));
        assert!(a.has_any_authority(&["delete", "read"]));
        assert!(!a.has_any_authority(&["delete", "erase"]));
        assert!(!a.has_any_authority(&[]));
    }

    #[test]
    fn error_display_matches_go_sentinels() {
        assert_eq!(
            SecurityError::Unauthenticated.to_string(),
            "firefly/security: unauthenticated"
        );
        assert_eq!(
            SecurityError::Forbidden.to_string(),
            "firefly/security: forbidden"
        );
        assert_eq!(
            SecurityError::MalformedHeader.to_string(),
            "malformed Authorization header"
        );
        assert_eq!(SecurityError::verification("nope").to_string(), "nope");
    }

    #[test]
    fn request_extension_roundtrip() {
        let req = http::Request::builder().uri("/x").body(()).unwrap();
        assert!(authentication_from(&req).is_none());
        let req = with_authentication(req, auth(&["USER"]));
        assert_eq!(authentication_from(&req).unwrap().username, "alice");
        assert_eq!(must_auth_from(&req).principal, "u1");
    }

    #[test]
    #[should_panic(expected = "firefly/security: no authentication on context")]
    fn must_auth_from_panics_without_auth() {
        let req = http::Request::builder().uri("/x").body(()).unwrap();
        let _ = must_auth_from(&req);
    }

    #[test]
    fn serde_roundtrip() {
        let mut a = auth(&["USER"]);
        a.claims
            .insert("scope".into(), serde_json::json!("read write"));
        let json = serde_json::to_string(&a).unwrap();
        let back: Authentication = serde_json::from_str(&json).unwrap();
        assert_eq!(back, a);
    }

    #[test]
    fn serde_defaults_missing_fields() {
        let a: Authentication = serde_json::from_str(r#"{"principal":"u9"}"#).unwrap();
        assert_eq!(a.principal, "u9");
        assert!(a.username.is_empty());
        assert!(a.roles.is_empty());
        assert!(a.claims.is_empty());
    }

    #[tokio::test]
    async fn verifier_fn_adapts_closures() {
        let v = VerifierFn(|token: String| async move {
            if token == "good" {
                Ok(Authentication::anonymous())
            } else {
                Err(SecurityError::verification("nope"))
            }
        });
        assert!(v.verify("good").await.is_ok());
        assert_eq!(
            v.verify("bad").await.unwrap_err(),
            SecurityError::verification("nope")
        );
    }
}
