//! firefly-ecm-storage-aws — the [`ContentStore`] adapter for the
//! corresponding cloud object store (AWS S3).
//!
//! Direct port of the Go module `fireflyframework-go/ecmstorageaws`, itself a
//! port of the Java `firefly-ecm-storage-aws` module and the .NET
//! `FireflyFramework.Ecm.Storage.*` project. The cloud SDK integration is in
//! scope for a later milestone — this crate ships the contract-only stub: the
//! types are declared, the port is satisfied, and sentinel-error smoke tests
//! guard the wire shape, but every method returns the
//! [`ERR_NOT_IMPLEMENTED`] sentinel.
//!
//! The sentinel message is bytes-equal to the Go port's
//! `ErrNotImplemented` (`firefly/ecmstorageaws: not yet implemented`),
//! carried through [`EcmError::Provider`] so consumers can match on the
//! rendered message exactly as Go callers match with `errors.Is`.
//!
//! # Quick start
//!
//! ```
//! use firefly_ecm::ContentStore;
//! use firefly_ecm_storage_aws::{Config, Store, ERR_NOT_IMPLEMENTED};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! let store = Store::new(Config {
//!     bucket: "firefly-docs".into(),
//!     region: "eu-west-1".into(),
//!     ..Default::default()
//! });
//!
//! assert_eq!(store.name(), "ecmstorageaws-stub");
//!
//! // Every method returns the sentinel until the cloud SDK is wired.
//! let err = store.delete("k").await.unwrap_err();
//! assert_eq!(err.to_string(), ERR_NOT_IMPLEMENTED);
//! # }
//! ```

use async_trait::async_trait;
use firefly_ecm::{ContentReader, ContentStore, EcmError};

/// The sentinel message returned by every method until the cloud SDK is
/// wired. Bytes-equal to the Go port's `ErrNotImplemented`:
///
/// ```go
/// var ErrNotImplemented = errors.New("firefly/ecmstorageaws: not yet implemented")
/// ```
pub const ERR_NOT_IMPLEMENTED: &str = "firefly/ecmstorageaws: not yet implemented";

/// Builds the not-yet-implemented sentinel as an [`EcmError::Provider`],
/// rendering [`ERR_NOT_IMPLEMENTED`] verbatim — the analog of returning Go's
/// `ErrNotImplemented`.
pub fn err_not_implemented() -> EcmError {
    EcmError::provider(ERR_NOT_IMPLEMENTED)
}

/// Config carries the wiring needed by the production adapter.
///
/// The fields cover every wiring variable the production adapter needs; the
/// Azure-flavoured fields exist because the Java module shares one
/// configuration surface across the cloud storage adapters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// Target S3 bucket name.
    pub bucket: String,
    /// AWS region (e.g. `eu-west-1`).
    pub region: String,
    /// AWS access key identifier.
    pub access_key: String,
    /// AWS secret access key.
    pub secret_key: String,
    /// Storage account name (Azure flavour of the shared surface).
    pub account: String,
    /// Account key (Azure flavour of the shared surface).
    pub key: String,
    /// Blob container name (Azure flavour of the shared surface).
    pub container: String,
    /// Optional custom endpoint (e.g. a LocalStack or MinIO URL).
    pub endpoint: String,
}

/// Store is the placeholder [`ContentStore`] adapter.
///
/// Construction succeeds and [`ContentStore::name`] answers, but
/// [`ContentStore::put`], [`ContentStore::get`], and [`ContentStore::delete`]
/// all return [`err_not_implemented`] until the production integration lands.
#[derive(Debug, Clone)]
pub struct Store {
    cfg: Config,
}

impl Store {
    /// Returns a placeholder Store.
    pub fn new(cfg: Config) -> Self {
        Self { cfg }
    }

    /// The configuration this store was constructed with, retained for the
    /// production adapter.
    pub fn config(&self) -> &Config {
        &self.cfg
    }
}

#[async_trait]
impl ContentStore for Store {
    /// Implements [`ContentStore::put`]; always [`err_not_implemented`].
    async fn put(&self, _key: &str, _content: ContentReader) -> Result<i64, EcmError> {
        Err(err_not_implemented())
    }

    /// Implements [`ContentStore::get`]; always [`err_not_implemented`].
    async fn get(&self, _key: &str) -> Result<ContentReader, EcmError> {
        Err(err_not_implemented())
    }

    /// Implements [`ContentStore::delete`]; always [`err_not_implemented`].
    async fn delete(&self, _key: &str) -> Result<(), EcmError> {
        Err(err_not_implemented())
    }

    /// Implements [`ContentStore::name`].
    fn name(&self) -> &str {
        "ecmstorageaws-stub"
    }
}

/// Framework version stamp.
pub const VERSION: &str = "26.6.1";

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use firefly_ecm::bytes_reader;

    /// Returns `true` when `err` is the not-yet-implemented sentinel — the
    /// analog of Go's `errors.Is(err, ErrNotImplemented)`.
    fn is_not_implemented(err: &EcmError) -> bool {
        matches!(err, EcmError::Provider(msg) if msg == ERR_NOT_IMPLEMENTED)
    }

    /// `Result::unwrap_err` without the `T: Debug` bound, which
    /// [`ContentReader`] does not carry.
    fn expect_err<T>(result: Result<T, EcmError>, op: &str) -> EcmError {
        match result {
            Ok(_) => panic!("{op}: expected the sentinel error, got Ok"),
            Err(err) => err,
        }
    }

    // -------------------------------------------------------------------
    // Ported from adapter_test.go
    // -------------------------------------------------------------------

    /// Go: `TestImplementsPort` — compile-time port satisfaction
    /// (`var _ ecm.ContentStore = New(Config{})`).
    #[test]
    fn implements_port() {
        fn assert_port<T: ContentStore>() {}
        assert_port::<Store>();

        // And the trait stays object-safe behind a box, like Go's interface.
        let _store: Box<dyn ContentStore> = Box::new(Store::new(Config::default()));
    }

    /// Go: `TestStubReturnsSentinel` — every method returns the sentinel,
    /// and the name is non-empty.
    #[tokio::test]
    async fn stub_returns_sentinel() {
        let s = Store::new(Config::default());

        let err = expect_err(s.put("k", bytes_reader(b"x".to_vec())).await, "Put");
        assert!(is_not_implemented(&err), "Put: {err}");

        let err = expect_err(s.get("k").await, "Get");
        assert!(is_not_implemented(&err), "Get: {err}");

        let err = expect_err(s.delete("k").await, "Delete");
        assert!(is_not_implemented(&err), "Delete: {err}");

        assert!(!s.name().is_empty(), "Name should be non-empty");
    }

    // -------------------------------------------------------------------
    // Rust-specific additions
    // -------------------------------------------------------------------

    #[test]
    fn sentinel_message_matches_go_bytes() {
        assert_eq!(
            ERR_NOT_IMPLEMENTED,
            "firefly/ecmstorageaws: not yet implemented"
        );
        let err = err_not_implemented();
        assert_eq!(
            err.to_string(),
            "firefly/ecmstorageaws: not yet implemented"
        );
        assert!(matches!(err, EcmError::Provider(_)));
        assert!(!err.is_not_found());
    }

    #[test]
    fn store_name_matches_go() {
        assert_eq!(Store::new(Config::default()).name(), "ecmstorageaws-stub");
    }

    #[test]
    fn config_is_retained() {
        let cfg = Config {
            bucket: "firefly-docs".into(),
            region: "eu-west-1".into(),
            access_key: "AKIA…".into(),
            secret_key: "s3cr3t".into(),
            account: "acct".into(),
            key: "key".into(),
            container: "blob".into(),
            endpoint: "http://localhost:4566".into(),
        };
        let store = Store::new(cfg.clone());
        assert_eq!(store.config(), &cfg);

        // Zero-value construction mirrors Go's `New(Config{})`.
        assert_eq!(Store::new(Config::default()).config(), &Config::default());
    }

    #[tokio::test]
    async fn usable_as_shared_trait_object() {
        let store: Arc<dyn ContentStore> = Arc::new(Store::new(Config::default()));
        assert_eq!(store.name(), "ecmstorageaws-stub");
        let err = expect_err(store.get("missing").await, "Get");
        assert!(is_not_implemented(&err));
        assert!(!err.is_not_found());
    }

    #[test]
    fn types_are_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Config>();
        assert_send_sync::<Store>();
        assert_send_sync::<Box<dyn ContentStore>>();
    }
}
