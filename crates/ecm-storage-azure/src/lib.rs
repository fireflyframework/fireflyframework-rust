//! firefly-ecm-storage-azure — Azure Blob Storage [`ContentStore`] adapter.
//!
//! This crate ships two flavours that share one [`Config`]:
//!
//! * [`BlobStore`] — the **real** adapter (pyfly parity). It speaks the Azure
//!   Blob REST API directly over [`reqwest`], authorizing every request with a
//!   self-contained **Shared Key** signer (see the [`sharedkey`] module —
//!   `hmac`/`sha2`/`base64`, no Azure SDK). It bridges
//!   [`firefly_ecm::ContentReader`] on both directions:
//!   [`ContentStore::put`] drains the reader and `PUT`s a block blob;
//!   [`ContentStore::get`] returns the blob body as a reader. It honours
//!   [`Config::endpoint`] so tests (and Azurite) can point it at an in-process
//!   mock server.
//! * [`Store`] — the original Go-parity **stub**, retained for backward
//!   compatibility. Every method returns the [`ERR_NOT_IMPLEMENTED`] sentinel,
//!   bytes-equal to the Go port's `ErrNotImplemented`
//!   (`firefly/ecmstorageazure: not yet implemented`).
//!
//! # Quick start (stub, back-compat)
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
//!
//! # Quick start (real adapter)
//!
//! ```no_run
//! use firefly_ecm::{bytes_reader, ContentStore};
//! use firefly_ecm_storage_azure::{BlobStore, Config};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), firefly_ecm::EcmError> {
//! let store = BlobStore::new(Config {
//!     account: "fireflyacct".into(),
//!     key: "Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==".into(),
//!     container: "documents".into(),
//!     ..Default::default()
//! })?;
//!
//! let n = store.put("doc-1/v1", bytes_reader(b"%PDF-1.7".to_vec())).await?;
//! assert_eq!(n, 8);
//! # Ok(())
//! # }
//! ```

pub mod sharedkey;

use async_trait::async_trait;
use chrono::Utc;
use firefly_ecm::{ContentReader, ContentStore, EcmError};
use tokio::io::AsyncReadExt;

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

/// The Azure Blob REST API version this adapter targets (sent as
/// `x-ms-version` on every request and folded into the Shared Key signature).
const X_MS_VERSION: &str = "2021-08-06";

/// Percent-encodes one blob-name path per RFC 3986: unreserved characters
/// (`A-Z a-z 0-9 - _ . ~`) and `/` pass through; every other byte becomes
/// `%XX` (uppercase hex). `/` is preserved so multi-segment blob names like
/// `doc-1/v1` keep their structure.
fn encode_blob(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for &b in name.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `BlobStore` is the production [`ContentStore`] adapter backed by the Azure
/// Blob REST API.
///
/// Construction builds a [`reqwest::Client`] and validates the [`Config`]
/// (`account`, `key`, and `container` are required; `key` must be base64).
/// Every request is authorized with the crate's self-contained [`sharedkey`]
/// signer; no Azure SDK is linked. The store is keyed by an opaque blob name —
/// the version-aware `<doc-id>/v<n>` scheme used by the ECM service maps
/// straight onto blob names.
///
/// # Endpoint resolution
///
/// * When [`Config::endpoint`] is set, requests go to
///   `{endpoint}/{container}/{blob}` (the form Azurite and the test mock server
///   use; the canonical resource still names the account).
/// * Otherwise the host `{account}.blob.core.windows.net` is used over HTTPS.
#[derive(Debug, Clone)]
pub struct BlobStore {
    cfg: Config,
    client: reqwest::Client,
    name: String,
}

impl BlobStore {
    /// Builds a real Azure-Blob-backed store from `cfg`.
    ///
    /// Returns an [`EcmError::Provider`] when the wiring is incomplete
    /// (`account`, `key`, `container` are required) or the HTTP client cannot
    /// be created.
    pub fn new(cfg: Config) -> Result<Self, EcmError> {
        if cfg.account.is_empty() {
            return Err(EcmError::provider(
                "firefly/ecmstorageazure: account is required",
            ));
        }
        if cfg.key.is_empty() {
            return Err(EcmError::provider(
                "firefly/ecmstorageazure: key is required",
            ));
        }
        if cfg.container.is_empty() {
            return Err(EcmError::provider(
                "firefly/ecmstorageazure: container is required",
            ));
        }
        let client = reqwest::Client::builder()
            .build()
            .map_err(|e| EcmError::provider(format!("firefly/ecmstorageazure: client: {e}")))?;
        Ok(Self {
            cfg,
            client,
            name: "azure-blob".to_string(),
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

    /// Returns `(url, host)` for the blob named `key`, honouring
    /// [`Config::endpoint`].
    fn endpoint(&self, key: &str) -> (String, String) {
        let encoded = encode_blob(key);
        if self.cfg.endpoint.is_empty() {
            let host = format!("{}.blob.core.windows.net", self.cfg.account);
            (
                format!("https://{host}/{}/{encoded}", self.cfg.container),
                host,
            )
        } else {
            let base = self.cfg.endpoint.trim_end_matches('/');
            let url = format!("{base}/{}/{encoded}", self.cfg.container);
            let host = host_of(&url);
            (url, host)
        }
    }

    /// Canonical resource for Shared Key signing — always names the account:
    /// `/<account>/<container>/<blob>`.
    fn canonical_resource(&self, key: &str) -> String {
        format!(
            "/{}/{}/{}",
            self.cfg.account,
            self.cfg.container,
            encode_blob(key)
        )
    }

    /// Signs and dispatches one request, returning the [`reqwest::Response`].
    async fn send(
        &self,
        method: reqwest::Method,
        key: &str,
        body: Option<Vec<u8>>,
    ) -> Result<reqwest::Response, EcmError> {
        let (url, host) = self.endpoint(key);
        let now = Utc::now();
        // RFC 1123 date in GMT, the format Azure requires for x-ms-date.
        let x_ms_date = now.format("%a, %d %b %Y %H:%M:%S GMT").to_string();

        let is_put = method == reqwest::Method::PUT;
        let content_len = body.as_ref().map(|b| b.len()).unwrap_or(0);
        // The 2015-02-21+ rule: send an empty Content-Length line unless there
        // is a body.
        let content_length = if content_len > 0 {
            content_len.to_string()
        } else {
            String::new()
        };
        let content_type = if is_put {
            "application/octet-stream"
        } else {
            ""
        };

        let mut x_ms_headers = vec![
            sharedkey::Header::new("x-ms-date", &x_ms_date),
            sharedkey::Header::new("x-ms-version", X_MS_VERSION),
        ];
        if is_put {
            x_ms_headers.push(sharedkey::Header::new("x-ms-blob-type", "BlockBlob"));
        }

        let sig_req = sharedkey::Request {
            method: method.as_str(),
            content_length: &content_length,
            content_type,
            x_ms_headers,
            canonical_resource: &self.canonical_resource(key),
        };
        let (authorization, _sig, _sts) =
            sharedkey::sign(&sig_req, &self.cfg.account, &self.cfg.key)
                .map_err(|e| EcmError::provider(format!("firefly/ecmstorageazure: sign: {e}")))?;

        let mut builder = self
            .client
            .request(method, &url)
            .header("host", &host)
            .header("x-ms-date", &x_ms_date)
            .header("x-ms-version", X_MS_VERSION)
            .header("authorization", &authorization);
        if is_put {
            builder = builder
                .header("x-ms-blob-type", "BlockBlob")
                .header("content-type", "application/octet-stream");
        }
        if let Some(b) = body {
            builder = builder.body(b);
        }
        builder
            .send()
            .await
            .map_err(|e| EcmError::provider(format!("firefly/ecmstorageazure: request: {e}")))
    }
}

/// Extracts the `host[:port]` authority from an `http(s)://host[:port]/...` URL
/// without a URL-parsing crate. Falls back to the whole input on odd shapes.
fn host_of(url: &str) -> String {
    let after_scheme = url.split("://").nth(1).unwrap_or(url);
    after_scheme
        .split('/')
        .next()
        .unwrap_or(after_scheme)
        .to_string()
}

#[async_trait]
impl ContentStore for BlobStore {
    /// Drains `content` into memory and `PUT`s it as a block blob at `key`,
    /// returning the number of bytes written. Surfaces any non-2xx response as
    /// an [`EcmError::Provider`].
    async fn put(&self, key: &str, mut content: ContentReader) -> Result<i64, EcmError> {
        let mut buf = Vec::new();
        content.read_to_end(&mut buf).await?;
        let len = buf.len() as i64;
        let resp = self.send(reqwest::Method::PUT, key, Some(buf)).await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(EcmError::provider(format!(
                "firefly/ecmstorageazure: put {key}: HTTP {}",
                status.as_u16()
            )));
        }
        Ok(len)
    }

    /// `GET`s the blob `key` and returns the body as a [`ContentReader`]. A
    /// `404` maps to [`EcmError::NotFound`]; any other non-2xx is an
    /// [`EcmError::Provider`].
    async fn get(&self, key: &str) -> Result<ContentReader, EcmError> {
        let resp = self.send(reqwest::Method::GET, key, None).await?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(EcmError::NotFound);
        }
        if !status.is_success() {
            return Err(EcmError::provider(format!(
                "firefly/ecmstorageazure: get {key}: HTTP {}",
                status.as_u16()
            )));
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| EcmError::provider(format!("firefly/ecmstorageazure: body: {e}")))?;
        Ok(firefly_ecm::bytes_reader(bytes.to_vec()))
    }

    /// `DELETE`s the blob `key`. A missing blob (`404`) is not an error
    /// (matching the port contract); any other non-2xx is an
    /// [`EcmError::Provider`].
    async fn delete(&self, key: &str) -> Result<(), EcmError> {
        let resp = self.send(reqwest::Method::DELETE, key, None).await?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        if !status.is_success() {
            return Err(EcmError::provider(format!(
                "firefly/ecmstorageazure: delete {key}: HTTP {}",
                status.as_u16()
            )));
        }
        Ok(())
    }

    /// Human-readable store identifier — `azure-blob`, matching pyfly's
    /// `AzureBlobStorageAdapter.name`.
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
