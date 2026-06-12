//! firefly-idp-azure-ad — the placeholder [`firefly_idp::Adapter`] for
//! Azure AD / Entra ID (MSAL + Microsoft Graph).
//!
//! Direct port of the Go `idpazuread` module (itself a port of the Java
//! `firefly-idp-azuread` module and the .NET `FireflyFramework.Idp.*`
//! project). The integration surface (token endpoints, Microsoft Graph
//! admin REST APIs, MSAL calls) is in scope for a later milestone — this
//! crate ships the contract-only **stub** today:
//!
//! * [`Config`] — typed wiring carried by the production adapter.
//! * [`Adapter`] — the placeholder implementation of the
//!   [`firefly_idp::Adapter`] port.
//! * [`ERR_NOT_IMPLEMENTED`] / [`not_implemented`] — the sentinel every
//!   method returns until the adapter ships, bytes-equal to the Go port's
//!   `idpazuread.ErrNotImplemented` (`firefly/idpazuread: not yet
//!   implemented`).
//!
//! # Why ship a stub?
//!
//! * The framework's tier diagram stays correct (no missing module).
//! * The port boundary stays locked — when the real implementation lands,
//!   no consuming code needs to change.
//! * The wire contract is exercised end-to-end before the integration
//!   ships, via the smoke tests that assert the sentinel return.
//!
//! # Quick start
//!
//! ```rust
//! use firefly_idp_azure_ad::{Adapter, Config, ERR_NOT_IMPLEMENTED};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! let idp: std::sync::Arc<dyn firefly_idp::Adapter> =
//!     std::sync::Arc::new(Adapter::new(Config::default()));
//! assert_eq!(idp.name(), "azuread-stub");
//!
//! // Every method returns the sentinel until the adapter ships.
//! let err = idp.login("u", "p").await.unwrap_err();
//! assert_eq!(err.to_string(), ERR_NOT_IMPLEMENTED);
//! # }
//! ```

use async_trait::async_trait;
use firefly_idp::{Error, Result, Token, User};

/// The sentinel message returned by every method until the adapter ships.
///
/// Bytes-equal to the Go port's `idpazuread.ErrNotImplemented` value so the
/// wire shape (error strings surfaced to callers and logs) is identical
/// across runtimes.
pub const ERR_NOT_IMPLEMENTED: &str = "firefly/idpazuread: not yet implemented";

/// Builds the not-yet-implemented sentinel as a [`firefly_idp::Error`].
///
/// The Go port models this as a package-level `errors.New` value; here it is
/// an [`Error::Provider`] carrying the same message, so callers can match it
/// with `err == not_implemented()` or compare the rendered string against
/// [`ERR_NOT_IMPLEMENTED`].
pub fn not_implemented() -> Error {
    Error::provider(ERR_NOT_IMPLEMENTED)
}

/// Carries the wiring needed by the production adapter.
///
/// The field set covers every wiring variable the Azure AD / Entra ID
/// integration needs (and mirrors the shared vendor-adapter config shape of
/// the Go port — unused fields stay empty for this provider).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// Base URL of the provider (authority host override).
    pub base_url: String,
    /// Realm / domain within the provider.
    pub realm: String,
    /// OAuth2 client (application) id.
    pub client_id: String,
    /// OAuth2 client secret.
    pub client_secret: String,
    /// Azure AD tenant id.
    pub tenant: String,
    /// User-pool id (shared vendor-config field; unused by Azure AD).
    pub user_pool_id: String,
    /// Cloud region (shared vendor-config field; unused by Azure AD).
    pub region: String,
}

/// The placeholder [`firefly_idp::Adapter`] for Azure AD / Entra ID.
///
/// Every port method returns the [`not_implemented`] sentinel; [`Self::config`]
/// exposes the wiring, and the port's `name()` reports `"azuread-stub"` so
/// wiring diagnostics make the stub status obvious.
#[derive(Debug, Clone)]
pub struct Adapter {
    cfg: Config,
}

impl Adapter {
    /// Returns a placeholder `Adapter` carrying the given wiring.
    pub fn new(cfg: Config) -> Self {
        Self { cfg }
    }

    /// Returns the wiring this adapter was constructed with.
    ///
    /// Rust-specific accessor (the Go port keeps the field private with no
    /// getter); handy for wiring inspection and diagnostics.
    pub fn config(&self) -> &Config {
        &self.cfg
    }
}

#[async_trait]
impl firefly_idp::Adapter for Adapter {
    /// Implements [`firefly_idp::Adapter::login`]; always returns the
    /// [`not_implemented`] sentinel.
    async fn login(&self, _username: &str, _password: &str) -> Result<Token> {
        Err(not_implemented())
    }

    /// Implements [`firefly_idp::Adapter::refresh`]; always returns the
    /// [`not_implemented`] sentinel.
    async fn refresh(&self, _refresh_token: &str) -> Result<Token> {
        Err(not_implemented())
    }

    /// Implements [`firefly_idp::Adapter::validate`]; always returns the
    /// [`not_implemented`] sentinel.
    async fn validate(&self, _access_token: &str) -> Result<User> {
        Err(not_implemented())
    }

    /// Implements [`firefly_idp::Adapter::get_user`]; always returns the
    /// [`not_implemented`] sentinel.
    async fn get_user(&self, _id: &str) -> Result<User> {
        Err(not_implemented())
    }

    /// Implements [`firefly_idp::Adapter::create_user`]; always returns the
    /// [`not_implemented`] sentinel.
    async fn create_user(&self, _user: User, _password: &str) -> Result<User> {
        Err(not_implemented())
    }

    /// Implements [`firefly_idp::Adapter::update_user`]; always returns the
    /// [`not_implemented`] sentinel.
    async fn update_user(&self, _user: User) -> Result<User> {
        Err(not_implemented())
    }

    /// Implements [`firefly_idp::Adapter::delete_user`]; always returns the
    /// [`not_implemented`] sentinel.
    async fn delete_user(&self, _id: &str) -> Result<()> {
        Err(not_implemented())
    }

    /// Implements [`firefly_idp::Adapter::name`].
    fn name(&self) -> &str {
        "azuread-stub"
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
        let _adapter: Arc<dyn firefly_idp::Adapter> = Arc::new(Adapter::new(Config::default()));
        let _boxed: Box<dyn firefly_idp::Adapter> = Box::new(Adapter::new(Config::default()));
    }

    // ---------------------------------------------------------------------
    // Port of Go TestStubReturnsSentinel: every method returns the
    // ErrNotImplemented sentinel and Name() is non-empty.
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn stub_returns_sentinel() {
        let a = Adapter::new(Config::default());
        let idp: &dyn firefly_idp::Adapter = &a;

        assert_eq!(
            idp.login("u", "p").await.unwrap_err(),
            not_implemented(),
            "login: want ErrNotImplemented"
        );
        assert_eq!(
            idp.refresh("tok").await.unwrap_err(),
            not_implemented(),
            "refresh"
        );
        assert_eq!(
            idp.validate("tok").await.unwrap_err(),
            not_implemented(),
            "validate"
        );
        assert_eq!(
            idp.get_user("u").await.unwrap_err(),
            not_implemented(),
            "get_user"
        );
        assert_eq!(
            idp.create_user(User::default(), "p").await.unwrap_err(),
            not_implemented(),
            "create_user"
        );
        assert_eq!(
            idp.update_user(User::default()).await.unwrap_err(),
            not_implemented(),
            "update_user"
        );
        assert_eq!(
            idp.delete_user("u").await.unwrap_err(),
            not_implemented(),
            "delete_user"
        );
        assert!(!idp.name().is_empty(), "name should be non-empty");
    }

    // ---------------------------------------------------------------------
    // Rust-specific guards.
    // ---------------------------------------------------------------------

    #[test]
    fn sentinel_message_matches_go() {
        // Wire-stable: bytes-equal to Go's idpazuread.ErrNotImplemented.
        assert_eq!(
            ERR_NOT_IMPLEMENTED,
            "firefly/idpazuread: not yet implemented"
        );
        assert_eq!(not_implemented().to_string(), ERR_NOT_IMPLEMENTED);
        assert_eq!(
            not_implemented(),
            Error::Provider("firefly/idpazuread: not yet implemented".into())
        );
    }

    #[test]
    fn sentinel_is_not_a_canonical_idp_sentinel() {
        // The stub's failure must never be confused with the canonical
        // login/lookup sentinels of the port.
        assert_ne!(not_implemented(), Error::InvalidCredentials);
        assert_ne!(not_implemented(), Error::UserNotFound);
    }

    #[test]
    fn name_is_stable() {
        let a = Adapter::new(Config::default());
        assert_eq!(firefly_idp::Adapter::name(&a), "azuread-stub");
    }

    #[test]
    fn config_round_trips_through_adapter() {
        let cfg = Config {
            base_url: "https://login.microsoftonline.com".into(),
            realm: "contoso.onmicrosoft.com".into(),
            client_id: "client".into(),
            client_secret: "secret".into(),
            tenant: "tenant-id".into(),
            user_pool_id: String::new(),
            region: String::new(),
        };
        let a = Adapter::new(cfg.clone());
        assert_eq!(a.config(), &cfg);
    }

    #[test]
    fn config_default_is_all_empty() {
        // Mirrors Go's Config{} zero value.
        let cfg = Config::default();
        assert!(cfg.base_url.is_empty());
        assert!(cfg.realm.is_empty());
        assert!(cfg.client_id.is_empty());
        assert!(cfg.client_secret.is_empty());
        assert!(cfg.tenant.is_empty());
        assert!(cfg.user_pool_id.is_empty());
        assert!(cfg.region.is_empty());
    }

    #[test]
    fn adapter_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Adapter>();
        assert_send_sync::<Config>();
        assert_send_sync::<Arc<dyn firefly_idp::Adapter>>();
    }
}
