//! firefly-ecm-storage-azure — the placeholder [`ContentStore`] adapter for
//! Azure Blob Storage.
//!
//! Faithful port of the Go module `fireflyframework-go/ecmstorageazure`: the
//! crate and types are declared, the port assertion compiles, and
//! sentinel-error smoke tests guard the wire shape — but the SaaS / cloud SDK
//! integration is **not yet wired**. Every [`ContentStore`] method returns the
//! [`ERR_NOT_IMPLEMENTED`] sentinel, rendered bytes-equal to the Go port's
//! `ErrNotImplemented`:
//!
//! ```text
//! firefly/ecmstorageazure: not yet implemented
//! ```
//!
//! # Why ship a stub?
//!
//! * The framework's tier diagram stays correct (no missing module).
//! * The port boundary stays locked — when the real implementation lands in
//!   v26.06, no consuming code needs to change.
//! * The wire contract is exercised end-to-end before the integration ships,
//!   via the smoke tests that assert the sentinel return.
//!
//! # Quick start
//!
//! ```
//! use firefly_ecm::{bytes_reader, ContentStore};
//! use firefly_ecm_storage_azure::{is_not_implemented, Config, Store};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! let store = Store::new(Config {
//!     account: "fireflyacct".into(),
//!     container: "documents".into(),
//!     ..Default::default()
//! });
//! assert_eq!(store.name(), "ecmstorageazure-stub");
//!
//! let err = store.put("k", bytes_reader(b"x".to_vec())).await.unwrap_err();
//! assert!(is_not_implemented(&err));
//! assert_eq!(err.to_string(), "firefly/ecmstorageazure: not yet implemented");
//! # }
//! ```

use async_trait::async_trait;
use firefly_ecm::{ContentReader, ContentStore, EcmError};

/// The sentinel message returned by every method until the cloud SDK is
/// wired. Bytes-equal to the Go port's `ErrNotImplemented`
/// (`firefly/ecmstorageazure: not yet implemented`).
pub const ERR_NOT_IMPLEMENTED: &str = "firefly/ecmstorageazure: not yet implemented";

/// Builds the not-yet-implemented sentinel as an [`EcmError::Provider`],
/// carrying [`ERR_NOT_IMPLEMENTED`] verbatim.
pub fn err_not_implemented() -> EcmError {
    EcmError::provider(ERR_NOT_IMPLEMENTED)
}

/// Returns `true` when `err` is this crate's not-yet-implemented sentinel —
/// the analog of Go's `errors.Is(err, ecmstorageazure.ErrNotImplemented)`.
pub fn is_not_implemented(err: &EcmError) -> bool {
    matches!(err, EcmError::Provider(msg) if msg == ERR_NOT_IMPLEMENTED)
}

/// Config carries the wiring needed by the production adapter.
///
/// Fields cover every wiring variable the production adapter needs; the
/// Azure-specific ones are `account`, `key`, and `container`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// Object-store bucket name.
    pub bucket: String,
    /// Cloud region identifier.
    pub region: String,
    /// Access-key credential.
    pub access_key: String,
    /// Secret-key credential.
    pub secret_key: String,
    /// Storage account name (azure).
    pub account: String,
    /// Storage account key (azure).
    pub key: String,
    /// Blob container name (azure).
    pub container: String,
    /// Custom service endpoint override.
    pub endpoint: String,
}

/// Store is the placeholder [`ContentStore`] adapter.
pub struct Store {
    cfg: Config,
}

impl Store {
    /// Returns a placeholder Store.
    pub fn new(cfg: Config) -> Self {
        Self { cfg }
    }

    /// Returns the wiring configuration captured at construction.
    pub fn config(&self) -> &Config {
        &self.cfg
    }
}

#[async_trait]
impl ContentStore for Store {
    /// Stub: always returns the [`ERR_NOT_IMPLEMENTED`] sentinel.
    async fn put(&self, _key: &str, _content: ContentReader) -> Result<i64, EcmError> {
        Err(err_not_implemented())
    }

    /// Stub: always returns the [`ERR_NOT_IMPLEMENTED`] sentinel.
    async fn get(&self, _key: &str) -> Result<ContentReader, EcmError> {
        Err(err_not_implemented())
    }

    /// Stub: always returns the [`ERR_NOT_IMPLEMENTED`] sentinel.
    async fn delete(&self, _key: &str) -> Result<(), EcmError> {
        Err(err_not_implemented())
    }

    /// Human-readable store identifier.
    fn name(&self) -> &str {
        "ecmstorageazure-stub"
    }
}

/// Framework version stamp.
pub const VERSION: &str = "26.6.1";

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use firefly_ecm::bytes_reader;

    // ---------------------------------------------------------------------
    // Go: TestImplementsPort — compile-time port satisfaction, expressed in
    // Rust as trait-object coercion behind Box and Arc.
    // ---------------------------------------------------------------------

    #[test]
    fn implements_port() {
        let _boxed: Box<dyn ContentStore> = Box::new(Store::new(Config::default()));
        let _shared: Arc<dyn ContentStore> = Arc::new(Store::new(Config::default()));
    }

    // ---------------------------------------------------------------------
    // Go: TestStubReturnsSentinel — every method returns ErrNotImplemented
    // and Name is non-empty.
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn stub_returns_sentinel() {
        let s = Store::new(Config::default());

        let err = s
            .put("k", bytes_reader(b"x".to_vec()))
            .await
            .expect_err("Put should fail");
        assert!(is_not_implemented(&err), "Put: {err}");

        let err = s.get("k").await.err().expect("Get should fail");
        assert!(is_not_implemented(&err), "Get: {err}");

        let err = s.delete("k").await.expect_err("Delete should fail");
        assert!(is_not_implemented(&err), "Delete: {err}");

        assert!(!s.name().is_empty(), "Name should be non-empty");
    }

    // ---------------------------------------------------------------------
    // Rust-specific: sentinel rendering, classification, and surface checks.
    // ---------------------------------------------------------------------

    #[test]
    fn sentinel_message_matches_go_bytes() {
        assert_eq!(
            ERR_NOT_IMPLEMENTED,
            "firefly/ecmstorageazure: not yet implemented"
        );
        assert_eq!(
            err_not_implemented().to_string(),
            "firefly/ecmstorageazure: not yet implemented"
        );
    }

    #[test]
    fn sentinel_is_provider_error_not_not_found() {
        let err = err_not_implemented();
        assert!(matches!(err, EcmError::Provider(_)));
        assert!(!err.is_not_found());
    }

    #[test]
    fn is_not_implemented_rejects_other_errors() {
        assert!(is_not_implemented(&err_not_implemented()));
        assert!(!is_not_implemented(&EcmError::NotFound));
        assert!(!is_not_implemented(&EcmError::provider(
            "firefly/ecmstorageaws: not yet implemented"
        )));
    }

    #[test]
    fn name_matches_go() {
        assert_eq!(Store::new(Config::default()).name(), "ecmstorageazure-stub");
    }

    #[test]
    fn config_is_captured_verbatim() {
        let cfg = Config {
            bucket: "b".into(),
            region: "westeurope".into(),
            access_key: "ak".into(),
            secret_key: "sk".into(),
            account: "fireflyacct".into(),
            key: "base64key".into(),
            container: "documents".into(),
            endpoint: "https://fireflyacct.blob.core.windows.net".into(),
        };
        let store = Store::new(cfg.clone());
        assert_eq!(store.config(), &cfg);
    }

    #[tokio::test]
    async fn usable_through_the_port_trait_object() {
        let store: Arc<dyn ContentStore> = Arc::new(Store::new(Config::default()));
        assert_eq!(store.name(), "ecmstorageazure-stub");
        let err = store
            .put("k", bytes_reader(Vec::new()))
            .await
            .expect_err("stub must not store");
        assert_eq!(
            err.to_string(),
            "firefly/ecmstorageazure: not yet implemented"
        );
    }

    #[test]
    fn store_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Store>();
        assert_send_sync::<Config>();
    }
}
