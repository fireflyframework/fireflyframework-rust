//! firefly-ecm-esignature-logalty — the placeholder Logalty
//! [`ESignatureProvider`] adapter (EU qualified e-signature, OAuth2
//! client_credentials).
//!
//! Faithful port of the Go module `fireflyframework-go/ecmesignaturelogalty`:
//! a contract-only stub. The crate and types are declared, the port assertion
//! compiles, and sentinel-error smoke tests guard the wire shape — but the
//! SaaS / cloud SDK integration is **not yet wired**. Every port method
//! returns the [`ERR_NOT_IMPLEMENTED`] sentinel, rendered byte-for-byte equal
//! to the Go port's `ErrNotImplemented`:
//!
//! ```text
//! firefly/ecmesignaturelogalty: not yet implemented
//! ```
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
//! ```
//! use firefly_ecm::{ESignatureProvider, SignatureRequest};
//! use firefly_ecm_esignature_logalty::{is_not_implemented, Config, Provider};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! let provider = Provider::new(Config::default());
//! assert_eq!(provider.name(), "ecmesignaturelogalty-stub");
//!
//! let err = provider.create(SignatureRequest::default()).await.unwrap_err();
//! assert!(is_not_implemented(&err));
//! assert_eq!(
//!     err.to_string(),
//!     "firefly/ecmesignaturelogalty: not yet implemented",
//! );
//! # }
//! ```

use async_trait::async_trait;
use firefly_ecm::{ESignatureProvider, EcmError, SignatureRequest, SignatureStatus};

/// Framework version stamp.
pub const VERSION: &str = "26.6.1";

/// The sentinel message returned by every method until the SaaS SDK is wired.
///
/// Byte-for-byte equal to the Go port's
/// `ErrNotImplemented = errors.New("firefly/ecmesignaturelogalty: not yet implemented")`.
pub const ERR_NOT_IMPLEMENTED: &str = "firefly/ecmesignaturelogalty: not yet implemented";

/// Builds the [`ERR_NOT_IMPLEMENTED`] sentinel as an [`EcmError::Provider`] —
/// the value every stubbed port method returns.
pub fn not_implemented() -> EcmError {
    EcmError::provider(ERR_NOT_IMPLEMENTED)
}

/// Returns `true` when `err` is the [`ERR_NOT_IMPLEMENTED`] sentinel — the
/// analog of Go's `errors.Is(err, ErrNotImplemented)`.
pub fn is_not_implemented(err: &EcmError) -> bool {
    matches!(err, EcmError::Provider(msg) if msg == ERR_NOT_IMPLEMENTED)
}

/// Config carries the OAuth2 / JWT-grant wiring needed by the production
/// adapter (Logalty — EU qualified e-signature, OAuth2 client_credentials).
///
/// The fields cover every wiring variable the production adapter needs; the
/// stub stores them untouched so consuming code can wire configuration today
/// and swap in the real adapter without changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// Logalty REST API base URL.
    pub base_url: String,
    /// OAuth2 client identifier (client_credentials grant).
    pub client_id: String,
    /// OAuth2 client secret (client_credentials grant).
    pub client_secret: String,
    /// Integration key issued for the Logalty tenant.
    pub integration_key: String,
    /// GUID of the impersonated Logalty user.
    pub user_guid: String,
}

/// Provider is the placeholder [`ESignatureProvider`] adapter.
///
/// Every port method returns the [`ERR_NOT_IMPLEMENTED`] sentinel until the
/// production Logalty integration is wired.
#[derive(Debug, Clone)]
pub struct Provider {
    cfg: Config,
}

impl Provider {
    /// Returns a placeholder Provider (the analog of Go's `New(cfg)`).
    pub fn new(cfg: Config) -> Self {
        Self { cfg }
    }

    /// Returns the configuration the provider was built with.
    pub fn config(&self) -> &Config {
        &self.cfg
    }
}

#[async_trait]
impl ESignatureProvider for Provider {
    /// Stubbed: always returns the [`ERR_NOT_IMPLEMENTED`] sentinel.
    async fn create(&self, _req: SignatureRequest) -> Result<String, EcmError> {
        Err(not_implemented())
    }

    /// Stubbed: always returns the [`ERR_NOT_IMPLEMENTED`] sentinel.
    async fn status(&self, _id: &str) -> Result<SignatureStatus, EcmError> {
        Err(not_implemented())
    }

    /// Stubbed: always returns the [`ERR_NOT_IMPLEMENTED`] sentinel.
    async fn cancel(&self, _id: &str) -> Result<(), EcmError> {
        Err(not_implemented())
    }

    /// Human-readable provider identifier, matching the Go stub.
    fn name(&self) -> &str {
        "ecmesignaturelogalty-stub"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // -----------------------------------------------------------------------
    // Go: TestImplementsPort — `var _ ecm.ESignatureProvider = New(Config{})`.
    // The Rust analog: the adapter coerces to the object-safe port behind
    // Arc/Box, which fails to compile if the trait is not implemented.
    // -----------------------------------------------------------------------

    #[test]
    fn implements_port() {
        let boxed: Box<dyn ESignatureProvider> = Box::new(Provider::new(Config::default()));
        assert_eq!(boxed.name(), "ecmesignaturelogalty-stub");

        let arc: Arc<dyn ESignatureProvider> = Arc::new(Provider::new(Config::default()));
        assert_eq!(arc.name(), "ecmesignaturelogalty-stub");
    }

    // -----------------------------------------------------------------------
    // Go: TestStubReturnsSentinel — every method returns ErrNotImplemented
    // and Name is non-empty.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn stub_returns_sentinel() {
        let p = Provider::new(Config::default());

        let err = p.create(SignatureRequest::default()).await.unwrap_err();
        assert!(is_not_implemented(&err), "Create: {err}");

        let err = p.status("id").await.unwrap_err();
        assert!(is_not_implemented(&err), "Status: {err}");

        let err = p.cancel("id").await.unwrap_err();
        assert!(is_not_implemented(&err), "Cancel: {err}");

        assert!(!p.name().is_empty(), "Name should be non-empty");
    }

    // -----------------------------------------------------------------------
    // Rust-specific: sentinel parity, error taxonomy, config plumbing, and
    // auto-trait bounds.
    // -----------------------------------------------------------------------

    #[test]
    fn sentinel_message_matches_go_byte_for_byte() {
        assert_eq!(
            ERR_NOT_IMPLEMENTED,
            "firefly/ecmesignaturelogalty: not yet implemented"
        );
        assert_eq!(
            not_implemented().to_string(),
            "firefly/ecmesignaturelogalty: not yet implemented"
        );
    }

    #[test]
    fn sentinel_is_provider_error_not_not_found() {
        let err = not_implemented();
        assert!(matches!(err, EcmError::Provider(_)));
        assert!(!err.is_not_found());
        assert!(is_not_implemented(&err));

        // Other errors are not mistaken for the sentinel.
        assert!(!is_not_implemented(&EcmError::NotFound));
        assert!(!is_not_implemented(&EcmError::provider("other failure")));
    }

    #[tokio::test]
    async fn create_with_populated_request_still_returns_sentinel() {
        let p = Provider::new(Config {
            base_url: "https://api.logalty.com".into(),
            client_id: "client".into(),
            client_secret: "secret".into(),
            integration_key: "ik-123".into(),
            user_guid: "guid-456".into(),
        });
        let err = p
            .create(SignatureRequest {
                document_id: "d1".into(),
                signers: vec!["a@example.com".into()],
                title: "NDA".into(),
                provider: "logalty".into(),
            })
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), ERR_NOT_IMPLEMENTED);
    }

    #[test]
    fn config_is_stored_untouched() {
        let cfg = Config {
            base_url: "https://api.logalty.com".into(),
            client_id: "client".into(),
            client_secret: "secret".into(),
            integration_key: "ik-123".into(),
            user_guid: "guid-456".into(),
        };
        let p = Provider::new(cfg.clone());
        assert_eq!(p.config(), &cfg);
    }

    #[test]
    fn config_default_is_all_empty() {
        let cfg = Config::default();
        assert!(cfg.base_url.is_empty());
        assert!(cfg.client_id.is_empty());
        assert!(cfg.client_secret.is_empty());
        assert!(cfg.integration_key.is_empty());
        assert!(cfg.user_guid.is_empty());
    }

    #[test]
    fn provider_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Provider>();
        assert_send_sync::<Config>();
        assert_send_sync::<Arc<dyn ESignatureProvider>>();
    }
}
