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

//! SOAP 1.1 client — a minimalist envelope-builder over `reqwest`.
//!
//! The Rust port of pyfly's `SoapClient`: it wraps the caller's body XML
//! in a SOAP 1.1 envelope, POSTs it as `text/xml; charset=utf-8` with an
//! optional `SOAPAction` header, and returns the raw response body. No
//! WSDL, no schema — exactly pyfly's "80% case" (production deployments
//! that need full WSDL support should reach for a dedicated SOAP stack).

use std::time::Duration;

use http::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_TYPE};

use firefly_kernel::{
    correlation_id, FireflyError, ProblemDetail, HEADER_CORRELATION_ID, PROBLEM_CONTENT_TYPE,
};
use firefly_observability::inject_headers;

use crate::error::ClientError;

/// Default per-request timeout (60 s), matching pyfly's `SoapClient`.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// The SOAP 1.1 envelope template — byte-for-byte identical to pyfly's
/// `_ENVELOPE`. `{body}` is replaced with the caller's body XML.
const ENVELOPE_PREFIX: &str = concat!(
    r#"<?xml version="1.0" encoding="UTF-8"?>"#,
    r#"<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">"#,
    "<soap:Header/>",
    "<soap:Body>",
);
const ENVELOPE_SUFFIX: &str = "</soap:Body></soap:Envelope>";

/// Wraps `body_xml` in the SOAP 1.1 envelope — exposed for callers that
/// want to inspect or log the exact wire payload `SoapClient::call`
/// would send.
#[must_use]
pub fn wrap_envelope(body_xml: &str) -> String {
    format!("{ENVELOPE_PREFIX}{body_xml}{ENVELOPE_SUFFIX}")
}

/// Fluently configures a [`SoapClient`] — the Rust analog of pyfly's
/// `SoapClientBuilder`.
///
/// ```
/// use firefly_client::SoapBuilder;
///
/// let client = SoapBuilder::new("https://soap.example.com/svc")
///     .with_action("GetThing")
///     .with_header("X-Auth", "abc")
///     .build();
/// # let _ = client;
/// ```
#[derive(Debug, Clone)]
pub struct SoapBuilder {
    endpoint: String,
    soap_action: String,
    headers: HeaderMap,
    timeout: Duration,
    http_client: Option<reqwest::Client>,
}

impl SoapBuilder {
    /// Returns a builder primed for the given SOAP endpoint.
    pub fn new(endpoint: impl AsRef<str>) -> Self {
        Self {
            endpoint: endpoint.as_ref().to_owned(),
            soap_action: String::new(),
            headers: HeaderMap::new(),
            timeout: DEFAULT_TIMEOUT,
            http_client: None,
        }
    }

    /// Sets the `SOAPAction` header value (pyfly's `with_action`). When
    /// empty the header is omitted, matching pyfly.
    #[must_use]
    pub fn with_action(mut self, action: impl AsRef<str>) -> Self {
        self.soap_action = action.as_ref().to_owned();
        self
    }

    /// Sets a default header forwarded on every call (pyfly's
    /// `with_header`), replacing any previous value for the same name.
    ///
    /// # Panics
    ///
    /// Panics when `key` is not a valid header name or `value` is not a
    /// valid header value — a programming error at wiring time.
    #[must_use]
    pub fn with_header(mut self, key: impl AsRef<str>, value: impl AsRef<str>) -> Self {
        let name = HeaderName::from_bytes(key.as_ref().as_bytes())
            .expect("SoapBuilder::with_header: invalid header name");
        let value = HeaderValue::from_str(value.as_ref())
            .expect("SoapBuilder::with_header: invalid header value");
        self.headers.insert(name, value);
        self
    }

    /// Overrides the per-request timeout (default 60 s, pyfly's default).
    /// Ignored when a custom client is injected via
    /// [`SoapBuilder::with_http_client`].
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Injects a custom [`reqwest::Client`], used as-is.
    #[must_use]
    pub fn with_http_client(mut self, client: reqwest::Client) -> Self {
        self.http_client = Some(client);
        self
    }

    /// Finalises the [`SoapClient`]. When no custom client was injected,
    /// a [`reqwest::Client`] is built with the configured timeout.
    pub fn build(self) -> SoapClient {
        let http = self.http_client.unwrap_or_else(|| {
            reqwest::Client::builder()
                .timeout(self.timeout)
                .build()
                .expect("SoapBuilder::build: reqwest client construction failed")
        });
        SoapClient {
            endpoint: self.endpoint,
            soap_action: self.soap_action,
            headers: self.headers,
            http,
        }
    }
}

/// An async SOAP 1.1 client built by [`SoapBuilder`] — the Rust analog
/// of pyfly's `SoapClient`.
#[derive(Debug, Clone)]
pub struct SoapClient {
    endpoint: String,
    soap_action: String,
    headers: HeaderMap,
    http: reqwest::Client,
}

impl SoapClient {
    /// Returns a builder primed for the given endpoint.
    pub fn builder(endpoint: impl AsRef<str>) -> SoapBuilder {
        SoapBuilder::new(endpoint)
    }

    /// Wraps `body_xml` in a SOAP 1.1 envelope, POSTs it as
    /// `text/xml; charset=utf-8` (with the `SOAPAction` header when one
    /// was configured), and returns the raw response body as a `String`.
    ///
    /// # Errors
    ///
    /// * [`ClientError::Problem`] for a non-2xx HTTP response (an
    ///   RFC 7807 body is decoded when present; otherwise the raw body
    ///   — typically a SOAP Fault — becomes the error detail).
    /// * [`ClientError::Transport`] for transport failures, including a
    ///   response body that is not valid UTF-8.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # async fn demo() -> Result<(), firefly_client::ClientError> {
    /// use firefly_client::SoapBuilder;
    ///
    /// let client = SoapBuilder::new("https://soap.example.com/svc")
    ///     .with_action("GetFoo")
    ///     .build();
    /// let xml = client.call("<GetFoo><id>42</id></GetFoo>").await?;
    /// assert!(!xml.is_empty());
    /// # Ok(())
    /// # }
    /// ```
    pub async fn call(&self, body_xml: &str) -> Result<String, ClientError> {
        let envelope = wrap_envelope(body_xml);

        let mut headers = self.headers.clone();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/xml; charset=utf-8"),
        );
        if !self.soap_action.is_empty() {
            if let Ok(value) = HeaderValue::from_str(&self.soap_action) {
                headers.insert(HeaderName::from_static("soapaction"), value);
            }
        }
        if let Some(id) = correlation_id() {
            if let (Ok(name), Ok(value)) = (
                HeaderName::from_bytes(HEADER_CORRELATION_ID.as_bytes()),
                HeaderValue::from_str(&id),
            ) {
                headers.insert(name, value);
            }
        }
        inject_headers(&mut headers);

        let resp = self
            .http
            .post(&self.endpoint)
            .headers(headers)
            .body(envelope)
            .send()
            .await
            .map_err(ClientError::Transport)?;

        let status = resp.status();
        let content_type = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let raw = resp.bytes().await.map_err(ClientError::Transport)?;

        if !status.is_success() {
            let mut ferr = FireflyError::new(
                String::new(),
                status.canonical_reason().unwrap_or_default(),
                status.as_u16(),
                String::from_utf8_lossy(&raw).into_owned(),
            );
            if content_type.starts_with(PROBLEM_CONTENT_TYPE) {
                if let Ok(pd) = serde_json::from_slice::<ProblemDetail>(&raw) {
                    ferr.code = pd.problem_type;
                    ferr.title = pd.title;
                    ferr.status = pd.status;
                    ferr.detail = pd.detail;
                    ferr.fields = pd.extensions;
                }
            }
            return Err(ClientError::Problem(ferr));
        }

        // `resp.text` semantics: decode as text, replacing any invalid
        // byte sequence rather than failing (pyfly's httpx does the same).
        Ok(String::from_utf8_lossy(&raw).into_owned())
    }
}
