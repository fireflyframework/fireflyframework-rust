// Copyright 2026 Firefly Software Foundation.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! firefly-ecm-storage-aws — AWS S3 [`ContentStore`] adapter.
//!
//! [`S3Store`] is the production adapter (pyfly parity). It speaks the S3 REST
//! API directly over [`reqwest`], signing every request with a self-contained
//! AWS Signature Version 4 implementation (see the [`sigv4`] module — no AWS SDK
//! is linked). It bridges [`firefly_ecm::ContentReader`] on both directions:
//!
//! * [`ContentStore::put`] drains the reader and issues a
//!   [`PutObject`](https://docs.aws.amazon.com/AmazonS3/latest/API/API_PutObject.html)
//!   `PUT /{bucket}/{key}`.
//! * [`ContentStore::get`] issues a
//!   [`GetObject`](https://docs.aws.amazon.com/AmazonS3/latest/API/API_GetObject.html)
//!   `GET /{bucket}/{key}` and returns the object body as a reader.
//! * [`ContentStore::delete`] issues a
//!   [`DeleteObject`](https://docs.aws.amazon.com/AmazonS3/latest/API/API_DeleteObject.html)
//!   `DELETE /{bucket}/{key}`.
//!
//! Every operation issues a real S3 REST call. The adapter honours
//! [`Config::endpoint`] so tests (and LocalStack / MinIO) can point it at an
//! in-process mock server or a local S3-compatible endpoint.
//!
//! # Quick start
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

    /// Returns `(url, host)` for the bucket itself (no object key) — used by
    /// bucket-scoped operations such as
    /// [`ListObjectsV2`](https://docs.aws.amazon.com/AmazonS3/latest/API/API_ListObjectsV2.html).
    fn bucket_endpoint(&self) -> (String, String) {
        if self.cfg.endpoint.is_empty() {
            let host = format!("{}.s3.{}.amazonaws.com", self.cfg.bucket, self.cfg.region);
            (format!("https://{host}/"), host)
        } else {
            let base = self.cfg.endpoint.trim_end_matches('/');
            let url = format!("{base}/{}/", self.cfg.bucket);
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

    /// Canonical request path for a bucket-scoped operation.
    fn bucket_canonical_uri(&self) -> String {
        if self.cfg.endpoint.is_empty() {
            "/".to_string()
        } else {
            format!("/{}/", self.cfg.bucket)
        }
    }

    /// Signs and dispatches one request described by `req`, returning the
    /// [`reqwest::Response`]. The request struct keeps the addressing, the
    /// canonical query, the extra signed headers, and the body together so the
    /// signed bytes and the bytes on the wire never diverge.
    async fn send_signed(&self, req: SignReq<'_>) -> Result<reqwest::Response, EcmError> {
        let SignReq {
            method,
            url,
            host,
            canonical_uri,
            canonical_query,
            extra_headers,
            body,
        } = req;

        let payload = body.as_deref().unwrap_or(b"");
        let payload_hash = sigv4::sha256_hex(payload);

        let now = Utc::now();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date_stamp = now.format("%Y%m%d").to_string();

        let mut headers = vec![
            sigv4::Header::new("host", host),
            sigv4::Header::new("x-amz-content-sha256", &payload_hash),
            sigv4::Header::new("x-amz-date", &amz_date),
        ];
        for (name, value) in extra_headers {
            headers.push(sigv4::Header::new(*name, value.clone()));
        }

        let sig_req = sigv4::Request {
            method: method.as_str(),
            canonical_uri,
            canonical_query,
            headers,
            payload_hash: &payload_hash,
        };
        let creds = sigv4::Credentials {
            access_key: &self.cfg.access_key,
            secret_key: &self.cfg.secret_key,
            region: &self.cfg.region,
            service: "s3",
        };
        let signed = sigv4::sign(&sig_req, &creds, &amz_date, &date_stamp);

        // Build the final URL by appending the *exact* canonical query that was
        // signed — never reqwest's `.query()` (which form-encodes differently),
        // so the bytes the server hashes match the bytes we signed.
        let full_url = if canonical_query.is_empty() {
            url.to_string()
        } else {
            format!("{url}?{canonical_query}")
        };

        let mut builder = self
            .client
            .request(method, &full_url)
            .header("host", host)
            .header("x-amz-content-sha256", &payload_hash)
            .header("x-amz-date", &amz_date)
            .header("authorization", &signed.authorization);
        for (name, value) in extra_headers {
            builder = builder.header(*name, value);
        }
        if let Some(b) = body {
            builder = builder.body(b);
        }
        builder
            .send()
            .await
            .map_err(|e| EcmError::provider(format!("firefly/ecmstorageaws: request: {e}")))
    }

    /// Signs and dispatches one object-scoped request with no extra headers or
    /// query — the path used by [`ContentStore::put`] / `get` / `delete`.
    async fn send(
        &self,
        method: reqwest::Method,
        key: &str,
        body: Option<Vec<u8>>,
    ) -> Result<reqwest::Response, EcmError> {
        let (url, host) = self.endpoint(key);
        let canonical_uri = self.canonical_uri(key);
        self.send_signed(SignReq {
            method,
            url: &url,
            host: &host,
            canonical_uri: &canonical_uri,
            canonical_query: "",
            extra_headers: &[],
            body,
        })
        .await
    }

    /// Lists object keys under `prefix` via
    /// [`ListObjectsV2`](https://docs.aws.amazon.com/AmazonS3/latest/API/API_ListObjectsV2.html)
    /// (`GET /{bucket}?list-type=2&prefix=…`), returning up to `max_keys`
    /// keys.
    ///
    /// This is a real S3 REST call: the request is SigV4-signed over the sorted
    /// canonical query string and the response `ListBucketResult` XML is parsed
    /// for the `<Key>` of every `<Contents>` entry. An empty `prefix` lists the
    /// whole bucket. Any non-2xx response is surfaced as an
    /// [`EcmError::Provider`].
    pub async fn list(&self, prefix: &str, max_keys: u32) -> Result<Vec<String>, EcmError> {
        let (url, host) = self.bucket_endpoint();
        // reqwest sends the query params; SigV4 signs the canonical (sorted,
        // percent-encoded) form of the same params.
        let max = max_keys.to_string();
        let mut params: Vec<(&str, &str)> = vec![("list-type", "2"), ("max-keys", &max)];
        if !prefix.is_empty() {
            params.push(("prefix", prefix));
        }
        let canonical_query = canonical_query(&params);

        let resp = self
            .send_signed(SignReq {
                method: reqwest::Method::GET,
                url: &url,
                host: &host,
                canonical_uri: &self.bucket_canonical_uri(),
                canonical_query: &canonical_query,
                extra_headers: &[],
                body: None,
            })
            .await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(EcmError::provider(format!(
                "firefly/ecmstorageaws: list {prefix}: HTTP {}",
                status.as_u16()
            )));
        }
        let xml = resp
            .text()
            .await
            .map_err(|e| EcmError::provider(format!("firefly/ecmstorageaws: body: {e}")))?;
        Ok(parse_list_keys(&xml))
    }

    /// Server-side copies `src_key` to `dst_key` within the same bucket via
    /// [`CopyObject`](https://docs.aws.amazon.com/AmazonS3/latest/API/API_CopyObject.html)
    /// (`PUT /{bucket}/{dst}` with `x-amz-copy-source: /{bucket}/{src}`).
    ///
    /// The copy never transfers bytes through the client — S3 performs it
    /// internally. The `x-amz-copy-source` header is part of the SigV4 signed
    /// header set. A `404` (missing source) maps to [`EcmError::NotFound`]; any
    /// other non-2xx is an [`EcmError::Provider`].
    pub async fn copy(&self, src_key: &str, dst_key: &str) -> Result<(), EcmError> {
        let (url, host) = self.endpoint(dst_key);
        let copy_source = format!("/{}/{}", self.cfg.bucket, encode_key(src_key));
        let resp = self
            .send_signed(SignReq {
                method: reqwest::Method::PUT,
                url: &url,
                host: &host,
                canonical_uri: &self.canonical_uri(dst_key),
                canonical_query: "",
                extra_headers: &[("x-amz-copy-source", copy_source)],
                body: None,
            })
            .await?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(EcmError::NotFound);
        }
        if !status.is_success() {
            return Err(EcmError::provider(format!(
                "firefly/ecmstorageaws: copy {src_key} -> {dst_key}: HTTP {}",
                status.as_u16()
            )));
        }
        Ok(())
    }

    /// Fetches object metadata for `key` via
    /// [`HeadObject`](https://docs.aws.amazon.com/AmazonS3/latest/API/API_HeadObject.html)
    /// (`HEAD /{bucket}/{key}`), returning the [`ObjectMetadata`] parsed from
    /// the response headers (`Content-Length`, `Content-Type`, `ETag`).
    ///
    /// A `404` maps to [`EcmError::NotFound`]; any other non-2xx is an
    /// [`EcmError::Provider`].
    pub async fn head(&self, key: &str) -> Result<ObjectMetadata, EcmError> {
        let resp = self.send(reqwest::Method::HEAD, key, None).await?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(EcmError::NotFound);
        }
        if !status.is_success() {
            return Err(EcmError::provider(format!(
                "firefly/ecmstorageaws: head {key}: HTTP {}",
                status.as_u16()
            )));
        }
        let header = |name: &str| {
            resp.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        Ok(ObjectMetadata {
            content_length: header("content-length")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            content_type: header("content-type").unwrap_or_default(),
            etag: header("etag").unwrap_or_default(),
        })
    }

    /// Builds a presigned `GET` URL for `key`, valid for `expires_secs`
    /// seconds, using
    /// [SigV4 query-string authentication](https://docs.aws.amazon.com/AmazonS3/latest/API/sigv4-query-string-auth.html).
    ///
    /// The returned URL carries the `X-Amz-Algorithm`, `X-Amz-Credential`,
    /// `X-Amz-Date`, `X-Amz-Expires`, `X-Amz-SignedHeaders`, and
    /// `X-Amz-Signature` query parameters, so any HTTP client can `GET` the
    /// object directly without further credentials. No network call is made —
    /// the URL is computed locally.
    pub fn presign_get(&self, key: &str, expires_secs: u32) -> Result<String, EcmError> {
        if !(1..=604_800).contains(&expires_secs) {
            return Err(EcmError::provider(
                "firefly/ecmstorageaws: presign expiry must be 1..=604800 seconds",
            ));
        }
        let (url, host) = self.endpoint(key);
        let canonical_uri = self.canonical_uri(key);

        let now = Utc::now();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date_stamp = now.format("%Y%m%d").to_string();

        let credential = format!(
            "{}/{}/{}/s3/aws4_request",
            self.cfg.access_key, date_stamp, self.cfg.region
        );
        // The signed query params, in canonical (sorted) order. Each value is
        // RFC-3986 encoded for both the canonical string and the final URL.
        let signed_headers = "host";
        let params = [
            ("X-Amz-Algorithm", "AWS4-HMAC-SHA256".to_string()),
            ("X-Amz-Credential", credential),
            ("X-Amz-Date", amz_date.clone()),
            ("X-Amz-Expires", expires_secs.to_string()),
            ("X-Amz-SignedHeaders", signed_headers.to_string()),
        ];
        let canonical_query = canonical_query(
            &params
                .iter()
                .map(|(k, v)| (*k, v.as_str()))
                .collect::<Vec<_>>(),
        );

        let creds = sigv4::Credentials {
            access_key: &self.cfg.access_key,
            secret_key: &self.cfg.secret_key,
            region: &self.cfg.region,
            service: "s3",
        };
        let signature = sigv4::presign_signature(
            "GET",
            &canonical_uri,
            &canonical_query,
            host.as_str(),
            &creds,
            &amz_date,
            &date_stamp,
        );

        Ok(format!(
            "{url}?{canonical_query}&X-Amz-Signature={signature}"
        ))
    }
}

/// The fully-described request fed to [`S3Store::send_signed`]: addressing, the
/// pre-built canonical query, the operation-specific signed headers, and the
/// optional body. Bundling these keeps the SigV4-signed bytes and the bytes
/// actually sent in lockstep.
struct SignReq<'a> {
    method: reqwest::Method,
    url: &'a str,
    host: &'a str,
    canonical_uri: &'a str,
    canonical_query: &'a str,
    extra_headers: &'a [(&'a str, String)],
    body: Option<Vec<u8>>,
}

/// Object metadata returned by [`S3Store::head`] — the subset of the
/// `HeadObject` response the ECM service needs (size, MIME type, ETag).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObjectMetadata {
    /// Object size in bytes (`Content-Length`).
    pub content_length: i64,
    /// Object MIME type (`Content-Type`).
    pub content_type: String,
    /// Entity tag (`ETag`), an opaque content fingerprint.
    pub etag: String,
}

/// Builds the canonical query string AWS SigV4 expects: each key and value
/// RFC-3986 percent-encoded, the pairs sorted by encoded key, joined with `&`.
/// Used by [`S3Store::list`] and the presigner.
fn canonical_query(params: &[(&str, &str)]) -> String {
    let mut encoded: Vec<(String, String)> = params
        .iter()
        .map(|(k, v)| (uri_encode(k, true), uri_encode(v, true)))
        .collect();
    encoded.sort();
    encoded
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// RFC-3986 "URI encode" per AWS's SigV4 rules: unreserved characters
/// (`A-Z a-z 0-9 - _ . ~`) pass through; every other byte becomes `%XX`
/// (uppercase hex). When `encode_slash` is false, `/` is preserved (used for
/// canonical paths); SigV4 query encoding always encodes `/`.
fn uri_encode(s: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b'/' if !encode_slash => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Extracts every `<Key>…</Key>` value from an S3 `ListBucketResult` XML body.
/// A minimal, dependency-free scan that pulls the keys out of each `<Contents>`
/// entry — sufficient for the [`ListObjectsV2`] response the adapter consumes.
fn parse_list_keys(xml: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<Key>") {
        let after = &rest[start + "<Key>".len()..];
        if let Some(end) = after.find("</Key>") {
            keys.push(xml_unescape(&after[..end]));
            rest = &after[end + "</Key>".len()..];
        } else {
            break;
        }
    }
    keys
}

/// Unescapes the five predefined XML entities that can appear in an S3 object
/// key inside a `ListBucketResult` body.
fn xml_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
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
pub const VERSION: &str = "26.6.14";

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn store() -> S3Store {
        S3Store::new(Config {
            bucket: "my-bucket".into(),
            region: "eu-west-1".into(),
            access_key: "AKIDEXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            ..Default::default()
        })
        .unwrap()
    }

    // -------------------------------------------------------------------
    // Port satisfaction — the real adapter is the `ContentStore`.
    // -------------------------------------------------------------------

    #[test]
    fn implements_port() {
        fn assert_port<T: ContentStore>() {}
        assert_port::<S3Store>();

        // And the trait stays object-safe behind a box, like Go's interface.
        let _store: Box<dyn ContentStore> = Box::new(store());
    }

    #[test]
    fn name_matches_pyfly() {
        assert_eq!(store().name(), "aws-s3");
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
        let store = S3Store::new(cfg.clone()).unwrap();
        assert_eq!(store.config(), &cfg);
    }

    // -------------------------------------------------------------------
    // Endpoint / addressing.
    // -------------------------------------------------------------------

    #[test]
    fn virtual_hosted_addressing_without_endpoint() {
        let (url, host) = store().endpoint("doc-1/v1");
        assert_eq!(host, "my-bucket.s3.eu-west-1.amazonaws.com");
        assert_eq!(url, "https://my-bucket.s3.eu-west-1.amazonaws.com/doc-1/v1");
        // Virtual-hosted canonical URI is just the key.
        assert_eq!(store().canonical_uri("doc-1/v1"), "/doc-1/v1");
    }

    #[test]
    fn path_style_addressing_with_endpoint() {
        let s = S3Store::new(Config {
            bucket: "my-bucket".into(),
            region: "eu-west-1".into(),
            access_key: "AKIDEXAMPLE".into(),
            secret_key: "secret".into(),
            endpoint: "http://localhost:4566".into(),
            ..Default::default()
        })
        .unwrap();
        let (url, host) = s.endpoint("doc-1/v1");
        assert_eq!(host, "localhost:4566");
        assert_eq!(url, "http://localhost:4566/my-bucket/doc-1/v1");
        // Path-style canonical URI names the bucket.
        assert_eq!(s.canonical_uri("doc-1/v1"), "/my-bucket/doc-1/v1");
    }

    #[test]
    fn keys_are_percent_encoded_preserving_slashes() {
        assert_eq!(encode_key("doc-1/v1"), "doc-1/v1");
        assert_eq!(encode_key("acme docs/v1"), "acme%20docs/v1");
        assert_eq!(encode_key("a+b=c"), "a%2Bb%3Dc");
    }

    #[test]
    fn canonical_query_is_sorted_and_encoded() {
        // ListObjectsV2 query: list-type fixed param plus an encoded prefix.
        assert_eq!(
            canonical_query(&[("prefix", "a b/"), ("list-type", "2")]),
            "list-type=2&prefix=a%20b%2F"
        );
    }

    // -------------------------------------------------------------------
    // Construction validation.
    // -------------------------------------------------------------------

    #[test]
    fn requires_complete_config() {
        assert!(S3Store::new(Config::default())
            .unwrap_err()
            .to_string()
            .contains("bucket"));
        assert!(S3Store::new(Config {
            bucket: "b".into(),
            ..Default::default()
        })
        .unwrap_err()
        .to_string()
        .contains("region"));
        assert!(S3Store::new(Config {
            bucket: "b".into(),
            region: "r".into(),
            ..Default::default()
        })
        .unwrap_err()
        .to_string()
        .contains("access_key"));
    }

    #[test]
    fn types_are_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Config>();
        assert_send_sync::<S3Store>();
        assert_send_sync::<ObjectMetadata>();
        assert_send_sync::<Box<dyn ContentStore>>();
        let _shared: Arc<dyn ContentStore> = Arc::new(store());
    }
}
