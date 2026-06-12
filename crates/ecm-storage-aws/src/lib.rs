//! firefly-ecm-storage-aws — AWS S3 [`ContentStore`] adapter.
//!
//! This crate ships two flavours that share one [`Config`]:
//!
//! * [`S3Store`] — the **real** adapter (pyfly parity). It speaks the S3 REST
//!   API directly over [`reqwest`], signing every request with a
//!   self-contained AWS Signature Version 4 implementation (see the [`sigv4`]
//!   module — no AWS SDK is linked). It bridges [`firefly_ecm::ContentReader`]
//!   on both directions: [`ContentStore::put`] drains the reader and `PUT`s the
//!   bytes; [`ContentStore::get`] returns the object body as a reader. It
//!   honours [`Config::endpoint`] so tests (and LocalStack / MinIO) can point
//!   it at an in-process mock server.
//! * [`Store`] — the original Go-parity **stub**, retained for backward
//!   compatibility. Every method returns the [`ERR_NOT_IMPLEMENTED`] sentinel,
//!   bytes-equal to the Go port's `ErrNotImplemented`
//!   (`firefly/ecmstorageaws: not yet implemented`).
//!
//! # Quick start (stub, back-compat)
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
//! // Every method returns the sentinel.
//! let err = store.delete("k").await.unwrap_err();
//! assert_eq!(err.to_string(), ERR_NOT_IMPLEMENTED);
//! # }
//! ```
//!
//! # Quick start (real adapter)
//!
//! ```no_run
//! use firefly_ecm::{bytes_reader, ContentStore};
//! use firefly_ecm_storage_aws::{Config, S3Store};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), firefly_ecm::EcmError> {
//! let store = S3Store::new(Config {
//!     bucket: "firefly-docs".into(),
//!     region: "eu-west-1".into(),
//!     access_key: "AKIA…".into(),
//!     secret_key: "s3cr3t".into(),
//!     ..Default::default()
//! })?;
//!
//! let n = store.put("doc-1/v1", bytes_reader(b"%PDF-1.7".to_vec())).await?;
//! assert_eq!(n, 8);
//! # Ok(())
//! # }
//! ```

pub mod sigv4;

use async_trait::async_trait;
use chrono::Utc;
use firefly_ecm::{ContentReader, ContentStore, EcmError};
use tokio::io::AsyncReadExt;

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

/// Percent-encodes one path segment per RFC 3986 / S3's "URI encode" rules:
/// unreserved characters (`A-Z a-z 0-9 - _ . ~`) and `/` pass through, every
/// other byte becomes `%XX` (uppercase hex). `/` is preserved so multi-segment
/// keys like `doc-1/v1` keep their structure.
fn encode_key(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    for &b in key.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `S3Store` is the production [`ContentStore`] adapter backed by the AWS S3
/// REST API.
///
/// Construction builds a [`reqwest::Client`] and validates the [`Config`].
/// Every request is signed with the crate's self-contained [`sigv4`] signer;
/// no AWS SDK is linked. The store is keyed exactly like the
/// [`firefly_ecm::ContentStore`] port — by an opaque object key — so the
/// version-aware `<doc-id>/v<n>` scheme used by the ECM service maps straight
/// onto S3 object keys.
///
/// # Endpoint resolution
///
/// * When [`Config::endpoint`] is set, requests go to
///   `{endpoint}/{bucket}/{key}` (path-style; the form LocalStack, MinIO, and
///   the test mock server use).
/// * Otherwise the virtual-hosted-style host
///   `{bucket}.s3.{region}.amazonaws.com` is used over HTTPS.
#[derive(Debug, Clone)]
pub struct S3Store {
    cfg: Config,
    client: reqwest::Client,
    name: String,
}

impl S3Store {
    /// Builds a real S3-backed store from `cfg`.
    ///
    /// Returns an [`EcmError::Provider`] when the wiring is incomplete
    /// (`bucket`, `region`, `access_key`, and `secret_key` are required) or the
    /// HTTP client cannot be created.
    pub fn new(cfg: Config) -> Result<Self, EcmError> {
        if cfg.bucket.is_empty() {
            return Err(EcmError::provider(
                "firefly/ecmstorageaws: bucket is required",
            ));
        }
        if cfg.region.is_empty() {
            return Err(EcmError::provider(
                "firefly/ecmstorageaws: region is required",
            ));
        }
        if cfg.access_key.is_empty() || cfg.secret_key.is_empty() {
            return Err(EcmError::provider(
                "firefly/ecmstorageaws: access_key and secret_key are required",
            ));
        }
        let client = reqwest::Client::builder()
            .build()
            .map_err(|e| EcmError::provider(format!("firefly/ecmstorageaws: client: {e}")))?;
        Ok(Self {
            cfg,
            client,
            name: "aws-s3".to_string(),
        })
    }

    /// Builds a store on top of an existing [`reqwest::Client`] — useful for
    /// tests and for sharing a connection pool / custom TLS config.
    pub fn with_client(cfg: Config, client: reqwest::Client) -> Result<Self, EcmError> {
        let mut s = Self::new(cfg)?;
        s.client = client;
        Ok(s)
    }

    /// The configuration this store was built with.
    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// Returns `(url, host)` for the object identified by `key`, honouring
    /// [`Config::endpoint`] for path-style addressing.
    fn endpoint(&self, key: &str) -> (String, String) {
        let encoded = encode_key(key);
        if self.cfg.endpoint.is_empty() {
            let host = format!("{}.s3.{}.amazonaws.com", self.cfg.bucket, self.cfg.region);
            (format!("https://{host}/{encoded}"), host)
        } else {
            let base = self.cfg.endpoint.trim_end_matches('/');
            let url = format!("{base}/{}/{encoded}", self.cfg.bucket);
            let host = host_of(&url);
            (url, host)
        }
    }

    /// Canonical request path for SigV4 — the bucket-and-key portion of the URL
    /// path, which differs between path-style and virtual-hosted addressing.
    fn canonical_uri(&self, key: &str) -> String {
        let encoded = encode_key(key);
        if self.cfg.endpoint.is_empty() {
            format!("/{encoded}")
        } else {
            format!("/{}/{encoded}", self.cfg.bucket)
        }
    }

    /// Signs and dispatches one request, returning the [`reqwest::Response`].
    async fn send(
        &self,
        method: reqwest::Method,
        key: &str,
        body: Option<Vec<u8>>,
    ) -> Result<reqwest::Response, EcmError> {
        let (url, host) = self.endpoint(key);
        let payload = body.as_deref().unwrap_or(b"");
        let payload_hash = sigv4::sha256_hex(payload);

        let now = Utc::now();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date_stamp = now.format("%Y%m%d").to_string();

        let sig_req = sigv4::Request {
            method: method.as_str(),
            canonical_uri: &self.canonical_uri(key),
            canonical_query: "",
            headers: vec![
                sigv4::Header::new("host", &host),
                sigv4::Header::new("x-amz-content-sha256", &payload_hash),
                sigv4::Header::new("x-amz-date", &amz_date),
            ],
            payload_hash: &payload_hash,
        };
        let creds = sigv4::Credentials {
            access_key: &self.cfg.access_key,
            secret_key: &self.cfg.secret_key,
            region: &self.cfg.region,
            service: "s3",
        };
        let signed = sigv4::sign(&sig_req, &creds, &amz_date, &date_stamp);

        let mut builder = self
            .client
            .request(method, &url)
            .header("host", &host)
            .header("x-amz-content-sha256", &payload_hash)
            .header("x-amz-date", &amz_date)
            .header("authorization", &signed.authorization);
        if let Some(b) = body {
            builder = builder.body(b);
        }
        builder
            .send()
            .await
            .map_err(|e| EcmError::provider(format!("firefly/ecmstorageaws: request: {e}")))
    }
}

/// Extracts the `host[:port]` authority from an `http(s)://host[:port]/...` URL
/// without pulling in a URL-parsing crate. Falls back to the whole input when
/// the shape is unexpected.
fn host_of(url: &str) -> String {
    let after_scheme = url.split("://").nth(1).unwrap_or(url);
    after_scheme
        .split('/')
        .next()
        .unwrap_or(after_scheme)
        .to_string()
}

#[async_trait]
impl ContentStore for S3Store {
    /// Drains `content` into memory, `PUT`s it to `key`, and returns the number
    /// of bytes written. Surfaces any non-2xx response as an
    /// [`EcmError::Provider`].
    async fn put(&self, key: &str, mut content: ContentReader) -> Result<i64, EcmError> {
        let mut buf = Vec::new();
        content.read_to_end(&mut buf).await?;
        let len = buf.len() as i64;
        let resp = self.send(reqwest::Method::PUT, key, Some(buf)).await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(EcmError::provider(format!(
                "firefly/ecmstorageaws: put {key}: HTTP {}",
                status.as_u16()
            )));
        }
        Ok(len)
    }

    /// `GET`s `key` and returns the body as a [`ContentReader`]. A `404` maps to
    /// [`EcmError::NotFound`]; any other non-2xx is an [`EcmError::Provider`].
    async fn get(&self, key: &str) -> Result<ContentReader, EcmError> {
        let resp = self.send(reqwest::Method::GET, key, None).await?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(EcmError::NotFound);
        }
        if !status.is_success() {
            return Err(EcmError::provider(format!(
                "firefly/ecmstorageaws: get {key}: HTTP {}",
                status.as_u16()
            )));
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| EcmError::provider(format!("firefly/ecmstorageaws: body: {e}")))?;
        Ok(firefly_ecm::bytes_reader(bytes.to_vec()))
    }

    /// `DELETE`s `key`. S3 returns `204` whether or not the object existed, so a
    /// missing key is not an error (matching the port contract); any other
    /// non-2xx is an [`EcmError::Provider`].
    async fn delete(&self, key: &str) -> Result<(), EcmError> {
        let resp = self.send(reqwest::Method::DELETE, key, None).await?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        if !status.is_success() {
            return Err(EcmError::provider(format!(
                "firefly/ecmstorageaws: delete {key}: HTTP {}",
                status.as_u16()
            )));
        }
        Ok(())
    }

    /// Human-readable store identifier — `aws-s3`, matching pyfly's
    /// `AwsS3StorageAdapter.name`.
    fn name(&self) -> &str {
        &self.name
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
