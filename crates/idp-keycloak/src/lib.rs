//! firefly-idp-keycloak — the placeholder [`idp::Adapter`] for Keycloak.
//!
//! Direct port of the Go `idpkeycloak` module (itself a port of the Java
//! `firefly-idp-keycloak` module and the .NET `FireflyFramework.Idp.*`
//! project). The integration surface (token endpoints, Keycloak admin REST
//! APIs) is in scope for a later milestone — this crate ships the
//! contract-only stub today, exactly as the Go module does.
//!
//! * [`Config`] — typed wiring for the production adapter (base URL, realm,
//!   client credentials, …).
//! * [`Adapter`] — the placeholder port implementation; every method returns
//!   the [`ERR_NOT_IMPLEMENTED`] sentinel.
//! * [`ERR_NOT_IMPLEMENTED`] / [`not_implemented`] — the wire-stable sentinel,
//!   bytes-equal to the Go module's `ErrNotImplemented`.
//!
//! # Why ship a stub?
//!
//! * The framework's tier diagram stays correct (no missing crate).
//! * The port boundary stays locked — when the real implementation lands,
//!   no consuming code needs to change.
//! * The wire contract is exercised end-to-end before the integration ships,
//!   via the smoke tests that assert the sentinel return.
//!
//! # Example
//!
//! ```
//! use firefly_idp::Adapter as _;
//! use firefly_idp_keycloak::{Adapter, Config, ERR_NOT_IMPLEMENTED};
//!
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let adapter = Adapter::new(Config {
//!     base_url: "https://keycloak.example.com".into(),
//!     realm: "firefly".into(),
//!     client_id: "firefly-app".into(),
//!     client_secret: "s3cret".into(),
//!     ..Config::default()
//! });
//! assert_eq!(adapter.name(), "keycloak-stub");
//!
//! let err = adapter.login("alice", "pw").await.unwrap_err();
//! assert_eq!(err.to_string(), ERR_NOT_IMPLEMENTED);
//! # });
//! ```

use async_trait::async_trait;
use firefly_idp as idp;
use firefly_idp::{Result, Token, User};

/// The sentinel message returned by every method until the adapter ships.
///
/// Bytes-equal to the Go module's `ErrNotImplemented`:
///
/// ```go
/// var ErrNotImplemented = errors.New("firefly/idpkeycloak: not yet implemented")
/// ```
pub const ERR_NOT_IMPLEMENTED: &str = "firefly/idpkeycloak: not yet implemented";

/// Builds the not-implemented sentinel as an [`idp::Error::Provider`] carrying
/// [`ERR_NOT_IMPLEMENTED`] verbatim — the Rust analog of comparing against the
/// Go `ErrNotImplemented` value with `errors.Is`.
pub fn not_implemented() -> idp::Error {
    idp::Error::Provider(ERR_NOT_IMPLEMENTED.to_string())
}

/// Typed configuration carrying the wiring needed by the production adapter.
///
/// Field-for-field port of the Go `Config` struct. The Keycloak adapter uses
/// `base_url`, `realm`, `client_id`, and `client_secret`; the remaining fields
/// (`tenant`, `user_pool_id`, `region`) mirror the shared vendor-stub shape so
/// the configuration surface is uniform across the IdP adapter family.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// Keycloak server base URL, e.g. `https://keycloak.example.com`.
    pub base_url: String,
    /// Keycloak realm the adapter authenticates against.
    pub realm: String,
    /// OIDC client identifier registered in the realm.
    pub client_id: String,
    /// OIDC client secret for confidential-client flows.
    pub client_secret: String,
    /// Vendor tenant identifier (used by sibling adapters; unused here).
    pub tenant: String,
    /// Vendor user-pool identifier (used by sibling adapters; unused here).
    pub user_pool_id: String,
    /// Vendor region (used by sibling adapters; unused here).
    pub region: String,
}

/// The placeholder [`idp::Adapter`] for Keycloak.
///
/// Every port method returns the [`ERR_NOT_IMPLEMENTED`] sentinel (wrapped in
/// [`idp::Error::Provider`]) until the production integration ships.
#[derive(Debug, Clone)]
pub struct Adapter {
    cfg: Config,
}

impl Adapter {
    /// Returns a placeholder [`Adapter`] holding the given wiring.
    pub fn new(cfg: Config) -> Self {
        Self { cfg }
    }

    /// Returns the configuration the adapter was constructed with.
    pub fn config(&self) -> &Config {
        &self.cfg
    }
}

#[async_trait]
impl idp::Adapter for Adapter {
    /// Implements [`idp::Adapter::login`]; always returns the sentinel.
    async fn login(&self, _username: &str, _password: &str) -> Result<Token> {
        Err(not_implemented())
    }

    /// Implements [`idp::Adapter::refresh`]; always returns the sentinel.
    async fn refresh(&self, _refresh_token: &str) -> Result<Token> {
        Err(not_implemented())
    }

    /// Implements [`idp::Adapter::validate`]; always returns the sentinel.
    async fn validate(&self, _access_token: &str) -> Result<User> {
        Err(not_implemented())
    }

    /// Implements [`idp::Adapter::get_user`]; always returns the sentinel.
    async fn get_user(&self, _id: &str) -> Result<User> {
        Err(not_implemented())
    }

    /// Implements [`idp::Adapter::create_user`]; always returns the sentinel.
    async fn create_user(&self, _user: User, _password: &str) -> Result<User> {
        Err(not_implemented())
    }

    /// Implements [`idp::Adapter::update_user`]; always returns the sentinel.
    async fn update_user(&self, _user: User) -> Result<User> {
        Err(not_implemented())
    }

    /// Implements [`idp::Adapter::delete_user`]; always returns the sentinel.
    async fn delete_user(&self, _id: &str) -> Result<()> {
        Err(not_implemented())
    }

    /// Implements [`idp::Adapter::name`]; returns `"keycloak-stub"`.
    fn name(&self) -> &str {
        "keycloak-stub"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // ---------------------------------------------------------------------
    // Port of Go TestImplementsPort: compile-time port satisfaction.
    // ---------------------------------------------------------------------

    #[test]
    fn implements_port() {
        let adapter: Arc<dyn idp::Adapter> = Arc::new(Adapter::new(Config::default()));
        let _ = &adapter;
    }

    // ---------------------------------------------------------------------
    // Port of Go TestStubReturnsSentinel: every method yields the sentinel.
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn stub_returns_sentinel() {
        use idp::Adapter as _;

        let a = Adapter::new(Config::default());

        assert_eq!(
            a.login("u", "p").await.unwrap_err(),
            not_implemented(),
            "Login: want ErrNotImplemented"
        );
        assert_eq!(
            a.refresh("tok").await.unwrap_err(),
            not_implemented(),
            "Refresh"
        );
        assert_eq!(
            a.validate("tok").await.unwrap_err(),
            not_implemented(),
            "Validate"
        );
        assert_eq!(
            a.get_user("u").await.unwrap_err(),
            not_implemented(),
            "GetUser"
        );
        assert_eq!(
            a.create_user(User::default(), "p").await.unwrap_err(),
            not_implemented(),
            "CreateUser"
        );
        assert_eq!(
            a.update_user(User::default()).await.unwrap_err(),
            not_implemented(),
            "UpdateUser"
        );
        assert_eq!(
            a.delete_user("u").await.unwrap_err(),
            not_implemented(),
            "DeleteUser"
        );
        assert!(!a.name().is_empty(), "Name should be non-empty");
    }

    // ---------------------------------------------------------------------
    // Rust-specific: sentinel wire shape, adapter name, config plumbing,
    // trait-object usability, and Send + Sync bounds.
    // ---------------------------------------------------------------------

    #[test]
    fn sentinel_message_matches_go() {
        assert_eq!(
            ERR_NOT_IMPLEMENTED,
            "firefly/idpkeycloak: not yet implemented"
        );
        assert_eq!(not_implemented().to_string(), ERR_NOT_IMPLEMENTED);
    }

    #[test]
    fn sentinel_is_provider_variant() {
        match not_implemented() {
            idp::Error::Provider(msg) => assert_eq!(msg, ERR_NOT_IMPLEMENTED),
            other => panic!("want Provider variant, got {other:?}"),
        }
    }

    #[test]
    fn name_matches_go() {
        use idp::Adapter as _;
        assert_eq!(Adapter::new(Config::default()).name(), "keycloak-stub");
    }

    #[test]
    fn config_round_trips_through_adapter() {
        let cfg = Config {
            base_url: "https://keycloak.example.com".into(),
            realm: "firefly".into(),
            client_id: "firefly-app".into(),
            client_secret: "s3cret".into(),
            tenant: "t1".into(),
            user_pool_id: "pool".into(),
            region: "eu-west-1".into(),
        };
        let adapter = Adapter::new(cfg.clone());
        assert_eq!(adapter.config(), &cfg);
    }

    #[test]
    fn config_default_is_all_empty() {
        let cfg = Config::default();
        assert!(cfg.base_url.is_empty());
        assert!(cfg.realm.is_empty());
        assert!(cfg.client_id.is_empty());
        assert!(cfg.client_secret.is_empty());
        assert!(cfg.tenant.is_empty());
        assert!(cfg.user_pool_id.is_empty());
        assert!(cfg.region.is_empty());
    }

    #[tokio::test]
    async fn usable_as_trait_object() {
        let adapter: Arc<dyn idp::Adapter> = Arc::new(Adapter::new(Config::default()));
        assert_eq!(adapter.name(), "keycloak-stub");
        assert_eq!(
            adapter.login("u", "p").await.unwrap_err(),
            not_implemented()
        );
    }

    #[test]
    fn adapter_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Adapter>();
        assert_send_sync::<Config>();
        assert_send_sync::<Arc<dyn idp::Adapter>>();
    }
}
