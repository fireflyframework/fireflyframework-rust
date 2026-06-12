//! firefly-idp-aws-cognito — the placeholder [`firefly_idp::Adapter`] for
//! AWS Cognito (AWS SDK `CognitoIdentityProvider`).
//!
//! Direct port of the Go `idpawscognito` module (itself a port of the Java
//! `firefly-idp-awscognito` module and the .NET `FireflyFramework.Idp.*`
//! project). The integration surface (token endpoints, admin REST APIs,
//! AWS SDK calls) is in scope for a later milestone — this crate ships the
//! contract-only stub today:
//!
//! * [`Config`] — typed wiring for the production adapter.
//! * [`Adapter`] — the placeholder port implementation; every method fails
//!   with the [`not_implemented`] sentinel.
//! * [`ERR_NOT_IMPLEMENTED`] — the wire-stable sentinel message, bytes-equal
//!   to the Go module's `ErrNotImplemented` error value.
//!
//! Shipping the stub keeps the framework's tier diagram correct (no missing
//! module) and locks the port boundary: when the real implementation lands,
//! no consuming code needs to change.
//!
//! # Example
//!
//! ```
//! use firefly_idp_aws_cognito::{not_implemented, Adapter, Config, ERR_NOT_IMPLEMENTED};
//!
//! let idp = Adapter::new(Config {
//!     user_pool_id: "eu-west-1_AbCdEfGhI".into(),
//!     region: "eu-west-1".into(),
//!     ..Config::default()
//! });
//! assert_eq!(firefly_idp::Adapter::name(&idp), "awscognito-stub");
//! assert_eq!(not_implemented().to_string(), ERR_NOT_IMPLEMENTED);
//! ```

use async_trait::async_trait;
use firefly_idp::{Error, Result, Token, User};

/// The wire-stable message carried by the [`not_implemented`] sentinel.
///
/// Bytes-equal to the Go module's `ErrNotImplemented` error value:
/// `errors.New("firefly/idpawscognito: not yet implemented")`.
pub const ERR_NOT_IMPLEMENTED: &str = "firefly/idpawscognito: not yet implemented";

/// Builds the sentinel error returned by every method until the adapter ships.
///
/// The sentinel travels as [`firefly_idp::Error::Provider`] with the
/// [`ERR_NOT_IMPLEMENTED`] message, so callers can match it with a plain
/// equality check:
///
/// ```
/// use firefly_idp_aws_cognito::{not_implemented, ERR_NOT_IMPLEMENTED};
///
/// let err = not_implemented();
/// assert_eq!(err, not_implemented());
/// assert_eq!(err.to_string(), ERR_NOT_IMPLEMENTED);
/// ```
pub fn not_implemented() -> Error {
    Error::provider(ERR_NOT_IMPLEMENTED)
}

/// Carries the wiring needed by the production adapter.
///
/// The fields cover every wiring variable the real AWS Cognito integration
/// needs; they are accepted (and retained) today so consuming configuration
/// code stays stable when the implementation lands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// Base URL of the provider endpoint.
    pub base_url: String,
    /// Authentication realm.
    pub realm: String,
    /// OAuth2 client identifier.
    pub client_id: String,
    /// OAuth2 client secret.
    pub client_secret: String,
    /// Tenant identifier.
    pub tenant: String,
    /// Cognito user-pool identifier.
    pub user_pool_id: String,
    /// AWS region hosting the user pool.
    pub region: String,
}

/// The placeholder [`firefly_idp::Adapter`] for AWS Cognito.
///
/// Every port method returns the [`not_implemented`] sentinel; the typed
/// [`Config`] is retained so wiring code compiles against the final surface
/// today.
#[derive(Debug, Clone)]
pub struct Adapter {
    cfg: Config,
}

impl Adapter {
    /// Returns a placeholder [`Adapter`] wired with `cfg`.
    pub fn new(cfg: Config) -> Self {
        Self { cfg }
    }

    /// Returns the wiring configuration the adapter was constructed with.
    pub fn config(&self) -> &Config {
        &self.cfg
    }
}

#[async_trait]
impl firefly_idp::Adapter for Adapter {
    /// Not yet implemented — always fails with the [`not_implemented`] sentinel.
    async fn login(&self, _username: &str, _password: &str) -> Result<Token> {
        Err(not_implemented())
    }

    /// Not yet implemented — always fails with the [`not_implemented`] sentinel.
    async fn refresh(&self, _refresh_token: &str) -> Result<Token> {
        Err(not_implemented())
    }

    /// Not yet implemented — always fails with the [`not_implemented`] sentinel.
    async fn validate(&self, _access_token: &str) -> Result<User> {
        Err(not_implemented())
    }

    /// Not yet implemented — always fails with the [`not_implemented`] sentinel.
    async fn get_user(&self, _id: &str) -> Result<User> {
        Err(not_implemented())
    }

    /// Not yet implemented — always fails with the [`not_implemented`] sentinel.
    async fn create_user(&self, _user: User, _password: &str) -> Result<User> {
        Err(not_implemented())
    }

    /// Not yet implemented — always fails with the [`not_implemented`] sentinel.
    async fn update_user(&self, _user: User) -> Result<User> {
        Err(not_implemented())
    }

    /// Not yet implemented — always fails with the [`not_implemented`] sentinel.
    async fn delete_user(&self, _id: &str) -> Result<()> {
        Err(not_implemented())
    }

    /// Returns the adapter's stable identifier: `"awscognito-stub"`.
    fn name(&self) -> &str {
        "awscognito-stub"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // ---------------------------------------------------------------------
    // Port of Go TestImplementsPort: compile-time port satisfaction
    // (`var _ idp.Adapter = New(Config{})`).
    // ---------------------------------------------------------------------

    #[test]
    fn implements_port() {
        fn assert_is_port<T: firefly_idp::Adapter>() {}
        assert_is_port::<Adapter>();
        let _: Arc<dyn firefly_idp::Adapter> = Arc::new(Adapter::new(Config::default()));
    }

    // ---------------------------------------------------------------------
    // Port of Go TestStubReturnsSentinel: every method returns the sentinel
    // and the adapter name is non-empty.
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn stub_returns_sentinel() {
        use firefly_idp::Adapter as _;

        let a = Adapter::new(Config::default());
        assert_eq!(
            a.login("u", "p").await.unwrap_err(),
            not_implemented(),
            "login: want ErrNotImplemented"
        );
        assert_eq!(a.refresh("tok").await.unwrap_err(), not_implemented());
        assert_eq!(a.validate("tok").await.unwrap_err(), not_implemented());
        assert_eq!(a.get_user("u").await.unwrap_err(), not_implemented());
        assert_eq!(
            a.create_user(User::default(), "p").await.unwrap_err(),
            not_implemented()
        );
        assert_eq!(
            a.update_user(User::default()).await.unwrap_err(),
            not_implemented()
        );
        assert_eq!(a.delete_user("u").await.unwrap_err(), not_implemented());
        assert!(!a.name().is_empty(), "name should be non-empty");
    }

    // ---------------------------------------------------------------------
    // Rust-specific: wire-stable sentinel message and adapter name.
    // ---------------------------------------------------------------------

    #[test]
    fn sentinel_message_matches_go() {
        let err = not_implemented();
        assert_eq!(
            err.to_string(),
            "firefly/idpawscognito: not yet implemented"
        );
        assert_eq!(err, Error::Provider(ERR_NOT_IMPLEMENTED.to_string()));
    }

    #[test]
    fn sentinel_is_distinct_from_canonical_port_errors() {
        let err = not_implemented();
        assert_ne!(err, Error::InvalidCredentials);
        assert_ne!(err, Error::UserNotFound);
    }

    #[test]
    fn name_is_stable() {
        let a = Adapter::new(Config::default());
        assert_eq!(firefly_idp::Adapter::name(&a), "awscognito-stub");
    }

    // ---------------------------------------------------------------------
    // Rust-specific: the typed Config is retained verbatim.
    // ---------------------------------------------------------------------

    #[test]
    fn config_is_retained() {
        let cfg = Config {
            base_url: "https://cognito-idp.eu-west-1.amazonaws.com".into(),
            realm: "main".into(),
            client_id: "cid".into(),
            client_secret: "secret".into(),
            tenant: "acme".into(),
            user_pool_id: "eu-west-1_AbCdEfGhI".into(),
            region: "eu-west-1".into(),
        };
        let a = Adapter::new(cfg.clone());
        assert_eq!(a.config(), &cfg);
    }

    #[test]
    fn default_config_mirrors_go_zero_values() {
        let cfg = Config::default();
        assert!(cfg.base_url.is_empty());
        assert!(cfg.realm.is_empty());
        assert!(cfg.client_id.is_empty());
        assert!(cfg.client_secret.is_empty());
        assert!(cfg.tenant.is_empty());
        assert!(cfg.user_pool_id.is_empty());
        assert!(cfg.region.is_empty());
    }

    // ---------------------------------------------------------------------
    // Rust-specific: usable behind Arc<dyn Adapter> and Send + Sync.
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn usable_as_trait_object() {
        let idp: Arc<dyn firefly_idp::Adapter> = Arc::new(Adapter::new(Config::default()));
        assert_eq!(idp.name(), "awscognito-stub");
        assert_eq!(idp.login("u", "p").await.unwrap_err(), not_implemented());
        assert_eq!(idp.delete_user("u").await.unwrap_err(), not_implemented());
    }

    #[test]
    fn adapter_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Adapter>();
        assert_send_sync::<Config>();
    }
}
