//! firefly-ecm-storage-azure — Azure Blob Storage [`ContentStore`] adapter.
//!
//! [`BlobStore`] is the production adapter (pyfly parity). It speaks the Azure
//! Blob REST API directly over [`reqwest`], authorizing every request with a
//! self-contained **Shared Key** signer (see the [`sharedkey`] module —
//! `hmac`/`sha2`/`base64`, no Azure SDK). It bridges
//! [`firefly_ecm::ContentReader`] on both directions:
//!
//! * [`ContentStore::put`] drains the reader and issues a
//!   [`Put Blob`](https://learn.microsoft.com/en-us/rest/api/storageservices/put-blob)
//!   block-blob upload (`PUT /{container}/{blob}`).
//! * [`ContentStore::get`] issues a
//!   [`Get Blob`](https://learn.microsoft.com/en-us/rest/api/storageservices/get-blob)
//!   `GET /{container}/{blob}` and returns the blob body as a reader.
//! * [`ContentStore::delete`] issues a
//!   [`Delete Blob`](https://learn.microsoft.com/en-us/rest/api/storageservices/delete-blob)
//!   `DELETE /{container}/{blob}`.
//!
//! Every operation issues a real Blob REST call. The adapter honours
//! [`Config::endpoint`] so tests (and Azurite) can point it at an in-process
//! mock server or a local emulator.
//!
//! # Quick start
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

    /// Canonical resource for Shared Key signing.
    ///
    /// Per the Shared Key spec the canonical resource is the credential
    /// **account name** followed by the **URL path** of the request:
    /// `/<account>` + `<url-path>`. Crucially the URL path is taken verbatim
    /// from the request that is actually sent, so the signature is correct for
    /// both endpoint styles:
    ///
    /// * **host-style** (real Azure, `https://<account>.blob.core.windows.net/
    ///   <container>/<blob>`): the path is `/<container>/<blob>`, giving
    ///   `/<account>/<container>/<blob>`.
    /// * **path-style** (Azurite / the emulator, `http://host/<account>/
    ///   <container>/<blob>`): the path already begins with the account, so the
    ///   canonical resource is `/<account>/<account>/<container>/<blob>` — which
    ///   is exactly what Azurite (and the path-style real-Azure form) compute.
    ///
    /// Deriving it from the URL path — rather than hard-coding
    /// `/<account>/<container>/<blob>` — is what makes the emulator round-trip
    /// authorize instead of 403.
    fn canonical_resource(&self, key: &str) -> String {
        let (url, _host) = self.endpoint(key);
        self.canonical_resource_for_url(&url)
    }

    /// Builds `/<account>` + the (already percent-encoded) path of `url`. The
    /// path is everything from the first `/` after the host; the leading `/` is
    /// kept so the result is `/<account>/<rest…>`.
    fn canonical_resource_for_url(&self, url: &str) -> String {
        format!("/{}{}", self.cfg.account, path_of(url))
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

    /// Lists blob names under `prefix` in the container via
    /// [`List Blobs`](https://learn.microsoft.com/en-us/rest/api/storageservices/list-blobs)
    /// (`GET /{container}?restype=container&comp=list&prefix=…`), returning every
    /// `<Name>` from the `EnumerationResults` XML.
    ///
    /// This is a real Blob REST call. The query parameters fold into the Shared
    /// Key canonical resource as sorted `\nparam:value` lines, so the signature
    /// covers them. An empty `prefix` lists the whole container. Any non-2xx
    /// response is surfaced as an [`EcmError::Provider`].
    pub async fn list(&self, prefix: &str) -> Result<Vec<String>, EcmError> {
        // Container-scoped resource (no blob); the list query params.
        let (base_url, host) = self.container_endpoint();
        // Canonical query params must be lowercase-name-sorted for the resource.
        let mut params: Vec<(&str, &str)> = vec![("comp", "list"), ("restype", "container")];
        if !prefix.is_empty() {
            params.push(("prefix", prefix));
        }
        params.sort_by(|a, b| a.0.cmp(b.0));

        let canonical_resource = self.container_canonical_resource(&base_url, &params);
        let query_string = params
            .iter()
            .map(|(k, v)| format!("{}={}", k, encode_query_value(v)))
            .collect::<Vec<_>>()
            .join("&");
        let url = format!("{base_url}?{query_string}");

        let resp = self
            .send_signed(
                reqwest::Method::GET,
                &url,
                &host,
                &canonical_resource,
                &[],
                None,
            )
            .await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(EcmError::provider(format!(
                "firefly/ecmstorageazure: list {prefix}: HTTP {}",
                status.as_u16()
            )));
        }
        let xml = resp
            .text()
            .await
            .map_err(|e| EcmError::provider(format!("firefly/ecmstorageazure: body: {e}")))?;
        Ok(parse_blob_names(&xml))
    }

    /// Server-side copies the blob `src_key` to `dst_key` within the same
    /// container via
    /// [`Copy Blob`](https://learn.microsoft.com/en-us/rest/api/storageservices/copy-blob)
    /// (`PUT /{container}/{dst}` with `x-ms-copy-source: <src URL>`).
    ///
    /// Azure performs the copy internally; no bytes flow through the client. The
    /// `x-ms-copy-source` header is part of the signed `x-ms-*` block. A `404`
    /// (missing source) maps to [`EcmError::NotFound`]; any other non-2xx is an
    /// [`EcmError::Provider`].
    pub async fn copy(&self, src_key: &str, dst_key: &str) -> Result<(), EcmError> {
        let (src_url, _) = self.endpoint(src_key);
        let (dst_url, host) = self.endpoint(dst_key);
        let resp = self
            .send_signed(
                reqwest::Method::PUT,
                &dst_url,
                &host,
                &self.canonical_resource(dst_key),
                &[("x-ms-copy-source", src_url)],
                None,
            )
            .await?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(EcmError::NotFound);
        }
        if !status.is_success() {
            return Err(EcmError::provider(format!(
                "firefly/ecmstorageazure: copy {src_key} -> {dst_key}: HTTP {}",
                status.as_u16()
            )));
        }
        Ok(())
    }

    /// Fetches blob metadata for `key` via
    /// [`Get Blob Properties`](https://learn.microsoft.com/en-us/rest/api/storageservices/get-blob-properties)
    /// (`HEAD /{container}/{blob}`), returning the [`BlobProperties`] parsed from
    /// the response headers (`Content-Length`, `Content-Type`, `ETag`).
    ///
    /// A `404` maps to [`EcmError::NotFound`]; any other non-2xx is an
    /// [`EcmError::Provider`].
    pub async fn properties(&self, key: &str) -> Result<BlobProperties, EcmError> {
        let (url, host) = self.endpoint(key);
        let resp = self
            .send_signed(
                reqwest::Method::HEAD,
                &url,
                &host,
                &self.canonical_resource(key),
                &[],
                None,
            )
            .await?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(EcmError::NotFound);
        }
        if !status.is_success() {
            return Err(EcmError::provider(format!(
                "firefly/ecmstorageazure: properties {key}: HTTP {}",
                status.as_u16()
            )));
        }
        let header = |name: &str| {
            resp.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        Ok(BlobProperties {
            content_length: header("content-length")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            content_type: header("content-type").unwrap_or_default(),
            etag: header("etag").unwrap_or_default(),
        })
    }

    /// Returns `(url, host)` for the container itself (no blob), used by the
    /// container-scoped [`List Blobs`] operation.
    fn container_endpoint(&self) -> (String, String) {
        if self.cfg.endpoint.is_empty() {
            let host = format!("{}.blob.core.windows.net", self.cfg.account);
            (format!("https://{host}/{}", self.cfg.container), host)
        } else {
            let base = self.cfg.endpoint.trim_end_matches('/');
            let url = format!("{base}/{}", self.cfg.container);
            let host = host_of(&url);
            (url, host)
        }
    }

    /// Builds the Shared Key canonical resource for a container-scoped request
    /// (e.g. List Blobs) with query params: the URL-derived
    /// `/<account>` + `<container-url-path>` (see [`Self::canonical_resource`]
    /// for why the path is taken from the URL, so both host-style and
    /// path-style endpoints sign correctly) followed by one
    /// `\n<lowercase-name>:<value>` line per param, sorted by name.
    fn container_canonical_resource(&self, base_url: &str, params: &[(&str, &str)]) -> String {
        let mut out = self.canonical_resource_for_url(base_url);
        let mut sorted: Vec<(String, String)> = params
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v.to_string()))
            .collect();
        sorted.sort();
        for (k, v) in sorted {
            out.push('\n');
            out.push_str(&k);
            out.push(':');
            out.push_str(&v);
        }
        out
    }

    /// Signs and dispatches one request against a fully-built `url`, with an
    /// explicit `canonical_resource` and optional extra signed `x-ms-*`
    /// headers. The lower-level path shared by [`Self::list`], [`Self::copy`],
    /// and [`Self::properties`]; the simple put/get/delete path uses
    /// [`Self::send`].
    async fn send_signed(
        &self,
        method: reqwest::Method,
        url: &str,
        host: &str,
        canonical_resource: &str,
        extra_x_ms: &[(&str, String)],
        body: Option<Vec<u8>>,
    ) -> Result<reqwest::Response, EcmError> {
        let now = Utc::now();
        let x_ms_date = now.format("%a, %d %b %Y %H:%M:%S GMT").to_string();

        let is_put = method == reqwest::Method::PUT;
        let content_len = body.as_ref().map(|b| b.len()).unwrap_or(0);
        let content_length = if content_len > 0 {
            content_len.to_string()
        } else {
            String::new()
        };
        // A Copy Blob PUT carries no body and therefore no content-type; a
        // body-bearing PUT (block blob) is octet-stream.
        let content_type = if is_put && content_len > 0 {
            "application/octet-stream"
        } else {
            ""
        };

        let mut x_ms_headers = vec![
            sharedkey::Header::new("x-ms-date", &x_ms_date),
            sharedkey::Header::new("x-ms-version", X_MS_VERSION),
        ];
        for (name, value) in extra_x_ms {
            x_ms_headers.push(sharedkey::Header::new(*name, value.clone()));
        }

        let sig_req = sharedkey::Request {
            method: method.as_str(),
            content_length: &content_length,
            content_type,
            x_ms_headers,
            canonical_resource,
        };
        let (authorization, _sig, _sts) =
            sharedkey::sign(&sig_req, &self.cfg.account, &self.cfg.key)
                .map_err(|e| EcmError::provider(format!("firefly/ecmstorageazure: sign: {e}")))?;

        let mut builder = self
            .client
            .request(method, url)
            .header("host", host)
            .header("x-ms-date", &x_ms_date)
            .header("x-ms-version", X_MS_VERSION)
            .header("authorization", &authorization);
        for (name, value) in extra_x_ms {
            builder = builder.header(*name, value);
        }
        if content_len > 0 {
            builder = builder.header("content-type", "application/octet-stream");
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

/// Blob metadata returned by [`BlobStore::properties`] — the subset of the
/// `Get Blob Properties` response the ECM service needs (size, MIME type,
/// ETag).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlobProperties {
    /// Blob size in bytes (`Content-Length`).
    pub content_length: i64,
    /// Blob MIME type (`Content-Type`).
    pub content_type: String,
    /// Entity tag (`ETag`), an opaque content fingerprint.
    pub etag: String,
}

/// Percent-encodes one query-parameter *value* for the List Blobs URL. Azure
/// accepts standard RFC-3986 encoding; `prefix` values like `docs/` keep their
/// `/` (it is a valid path-like prefix that the service matches literally).
fn encode_query_value(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for &b in v.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Extracts every `<Name>…</Name>` value from an Azure `EnumerationResults`
/// (List Blobs) XML body. A minimal, dependency-free scan over each `<Blob>`
/// entry's name element.
fn parse_blob_names(xml: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<Name>") {
        let after = &rest[start + "<Name>".len()..];
        if let Some(end) = after.find("</Name>") {
            names.push(xml_unescape(&after[..end]));
            rest = &after[end + "</Name>".len()..];
        } else {
            break;
        }
    }
    names
}

/// Unescapes the five predefined XML entities that can appear in a blob name
/// inside an `EnumerationResults` body.
fn xml_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
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

/// Extracts the path component (everything from the first `/` after the host,
/// **including** that leading `/`, but excluding any `?query`) from an
/// `http(s)://host[:port]/path?query` URL, without a URL-parsing crate. The
/// path is returned exactly as it appears in the URL (already percent-encoded),
/// because the Shared Key canonical resource is built over the encoded path.
/// Returns `/` when the URL has no path segment.
fn path_of(url: &str) -> String {
    let after_scheme = url.split("://").nth(1).unwrap_or(url);
    // Everything after the authority, up to an optional query string.
    let path = match after_scheme.find('/') {
        Some(i) => &after_scheme[i..],
        None => "/",
    };
    path.split('?').next().unwrap_or(path).to_string()
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

    const ACCOUNT: &str = "devstoreaccount1";
    const KEY: &str =
        "Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==";

    fn store() -> BlobStore {
        BlobStore::new(Config {
            account: ACCOUNT.into(),
            key: KEY.into(),
            container: "my-container".into(),
            ..Default::default()
        })
        .unwrap()
    }

    // ---------------------------------------------------------------------
    // Port satisfaction — the real adapter is the `ContentStore`.
    // ---------------------------------------------------------------------

    #[test]
    fn implements_port() {
        let _boxed: Box<dyn ContentStore> = Box::new(store());
        let _shared: Arc<dyn ContentStore> = Arc::new(store());
    }

    #[test]
    fn name_matches_pyfly() {
        assert_eq!(store().name(), "azure-blob");
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
        let store = BlobStore::new(cfg.clone()).unwrap();
        assert_eq!(store.config(), &cfg);
    }

    // ---------------------------------------------------------------------
    // Construction validation.
    // ---------------------------------------------------------------------

    #[test]
    fn requires_complete_config() {
        assert!(BlobStore::new(Config::default())
            .unwrap_err()
            .to_string()
            .contains("account"));
        assert!(BlobStore::new(Config {
            account: "a".into(),
            ..Default::default()
        })
        .unwrap_err()
        .to_string()
        .contains("key"));
        assert!(BlobStore::new(Config {
            account: "a".into(),
            key: "k".into(),
            ..Default::default()
        })
        .unwrap_err()
        .to_string()
        .contains("container"));
    }

    // ---------------------------------------------------------------------
    // Endpoint / canonical resource / encoding.
    // ---------------------------------------------------------------------

    #[test]
    fn endpoint_uses_account_host_without_override() {
        let (url, host) = store().endpoint("doc-1/v1");
        assert_eq!(host, "devstoreaccount1.blob.core.windows.net");
        assert_eq!(
            url,
            "https://devstoreaccount1.blob.core.windows.net/my-container/doc-1/v1"
        );
    }

    #[test]
    fn canonical_resource_always_names_account() {
        // No endpoint override -> host-style URL whose path is just
        // `/<container>/<blob>`, so the canonical resource names the account
        // once: `/<account>/<container>/<blob>`.
        assert_eq!(
            store().canonical_resource("doc-1/v1"),
            "/devstoreaccount1/my-container/doc-1/v1"
        );
        // Container-scoped list resource folds sorted query params in.
        let (base_url, _host) = store().container_endpoint();
        assert_eq!(
            store().container_canonical_resource(
                &base_url,
                &[
                    ("restype", "container"),
                    ("comp", "list"),
                    ("prefix", "docs/"),
                ]
            ),
            "/devstoreaccount1/my-container\ncomp:list\nprefix:docs/\nrestype:container"
        );
    }

    #[test]
    fn canonical_resource_doubles_account_for_path_style_endpoint() {
        // A path-style (emulator / Azurite) endpoint whose URL path *already*
        // begins with the account name must produce `/<account>/<account>/…`,
        // matching what Azurite computes — this is the fix for the 403.
        let store = BlobStore::new(Config {
            account: ACCOUNT.into(),
            key: KEY.into(),
            container: "my-container".into(),
            endpoint: format!("http://127.0.0.1:10000/{ACCOUNT}"),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            store.canonical_resource("doc-1/v1"),
            "/devstoreaccount1/devstoreaccount1/my-container/doc-1/v1"
        );
        let (base_url, _host) = store.container_endpoint();
        assert_eq!(
            store.container_canonical_resource(&base_url, &[("restype", "container")]),
            "/devstoreaccount1/devstoreaccount1/my-container\nrestype:container"
        );
    }

    #[test]
    fn path_of_extracts_url_path_without_query() {
        assert_eq!(path_of("https://h.example.com/c/b"), "/c/b");
        assert_eq!(path_of("http://127.0.0.1:10000/acct/c/b"), "/acct/c/b");
        assert_eq!(
            path_of("http://h/acct/c?restype=container&comp=list"),
            "/acct/c"
        );
        assert_eq!(path_of("https://h.example.com"), "/");
    }

    #[test]
    fn blob_names_are_percent_encoded_preserving_slashes() {
        assert_eq!(encode_blob("doc-1/v1"), "doc-1/v1");
        assert_eq!(encode_blob("acme docs/v1"), "acme%20docs/v1");
    }

    #[test]
    fn parses_blob_names_from_enumeration_results() {
        let xml = concat!(
            "<EnumerationResults><Blobs>",
            "<Blob><Name>docs/a/v1</Name></Blob>",
            "<Blob><Name>docs/b/v1</Name></Blob>",
            "</Blobs></EnumerationResults>"
        );
        assert_eq!(parse_blob_names(xml), vec!["docs/a/v1", "docs/b/v1"]);
    }

    #[test]
    fn store_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<BlobStore>();
        assert_send_sync::<Config>();
        assert_send_sync::<BlobProperties>();
    }
}
