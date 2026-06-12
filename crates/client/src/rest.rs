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

//! Fluent REST client builder over `reqwest`.

use std::time::Duration;

use http::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, CONTENT_TYPE};
use http::Method;
use serde::de::DeserializeOwned;
use serde::Serialize;

use firefly_kernel::{
    correlation_id, FireflyError, ProblemDetail, HEADER_CORRELATION_ID, PROBLEM_CONTENT_TYPE,
};
use firefly_observability::inject_headers;

use crate::error::ClientError;

/// Convenience placeholder for body-less requests, so call sites read
/// `client.request(Method::GET, "/x", NO_BODY)` instead of spelling out
/// the `Option::<&()>::None` turbofish.
pub const NO_BODY: Option<&()> = None;

/// Default per-request timeout (10 s), matching the Go port.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
/// Default total attempt budget (3), matching the Go port.
const DEFAULT_RETRIES: usize = 3;
/// Default first-retry backoff delay (100 ms), matching the Go port.
const DEFAULT_BACKOFF_BASE: Duration = Duration::from_millis(100);
/// Exponential backoff ceiling (2 s), matching the Go port.
const BACKOFF_CAP: Duration = Duration::from_secs(2);

/// Returns a [`RestBuilder`] primed for the given base URL — the Rust
/// spelling of Go's `client.NewREST(baseURL)`.
pub fn new_rest(base_url: impl AsRef<str>) -> RestBuilder {
    RestBuilder::new(base_url)
}

/// Fluently configures a [`RestClient`].
///
/// Mirrors the Go `RESTBuilder`: base URL (trailing slashes trimmed),
/// default headers, per-request timeout (default 10 s), attempt budget
/// (default 3), and an optional injected [`reqwest::Client`].
#[derive(Debug, Clone)]
pub struct RestBuilder {
    base_url: String,
    headers: HeaderMap,
    timeout: Duration,
    http_client: Option<reqwest::Client>,
    retry_max: usize,
    backoff_base: Duration,
}

impl RestBuilder {
    /// Returns a builder primed for the given base URL. Trailing `/`
    /// characters are trimmed so `base + path` concatenation stays
    /// clean, exactly as in the Go port.
    pub fn new(base_url: impl AsRef<str>) -> Self {
        Self {
            base_url: base_url.as_ref().trim_end_matches('/').to_owned(),
            headers: HeaderMap::new(),
            timeout: DEFAULT_TIMEOUT,
            http_client: None,
            retry_max: DEFAULT_RETRIES,
            backoff_base: DEFAULT_BACKOFF_BASE,
        }
    }

    /// Sets a default request header, replacing any previous value for
    /// the same name (Go's `http.Header.Set` semantics).
    ///
    /// # Panics
    ///
    /// Panics when `key` is not a valid HTTP header name or `value` is
    /// not a valid header value — a programming error at wiring time.
    #[must_use]
    pub fn with_header(mut self, key: impl AsRef<str>, value: impl AsRef<str>) -> Self {
        let name = HeaderName::from_bytes(key.as_ref().as_bytes())
            .expect("RestBuilder::with_header: invalid header name");
        let value = HeaderValue::from_str(value.as_ref())
            .expect("RestBuilder::with_header: invalid header value");
        self.headers.insert(name, value);
        self
    }

    /// Overrides the per-request timeout (default 10 s). Ignored when a
    /// custom client is injected via [`RestBuilder::with_http_client`],
    /// which is used as-is — the same contract as the Go port.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Injects a custom [`reqwest::Client`] — the analog of Go's
    /// `WithHTTPClient(*http.Client)`. The injected client is used
    /// as-is, so its own timeout configuration wins.
    #[must_use]
    pub fn with_http_client(mut self, client: reqwest::Client) -> Self {
        self.http_client = Some(client);
        self
    }

    /// Overrides the total attempt budget (default 3). The value is the
    /// number of attempts, not the number of retries: `1` means a
    /// single attempt with no retry, and `0` means [`RestClient`] calls
    /// fail with [`ClientError::Exhausted`] without sending anything.
    #[must_use]
    pub fn with_retries(mut self, attempts: usize) -> Self {
        self.retry_max = attempts;
        self
    }

    /// Overrides the first-retry backoff delay (default 100 ms; each
    /// retry doubles it, capped at 2 s). A Rust-specific extension —
    /// the Go port hard-codes 100 ms — kept so tests and latency-bound
    /// callers can tighten the schedule without changing its shape.
    #[must_use]
    pub fn with_backoff_base(mut self, base: Duration) -> Self {
        self.backoff_base = base;
        self
    }

    /// Finalises the client. When no custom client was injected, a
    /// [`reqwest::Client`] is built with the configured timeout.
    pub fn build(self) -> RestClient {
        let http = self.http_client.unwrap_or_else(|| {
            reqwest::Client::builder()
                .timeout(self.timeout)
                .build()
                .expect("RestBuilder::build: reqwest client construction failed")
        });
        RestClient {
            base: self.base_url,
            headers: self.headers,
            http,
            retry_max: self.retry_max,
            backoff_base: self.backoff_base,
        }
    }
}

/// A JSON-over-HTTP client built by [`RestBuilder`] — the Rust analog
/// of Go's `RESTClient` with its single `Do(ctx, method, path, body,
/// out)` method, split into [`RestClient::request`] (typed JSON decode)
/// and [`RestClient::send`] (raw bytes, Go's `out == nil`).
///
/// Every request automatically:
///
/// * JSON-encodes the body (when present) and sets
///   `Content-Type: application/json`;
/// * sets `Accept: application/json`;
/// * forwards the correlation id from the kernel task-local scope as
///   `X-Correlation-Id`, plus the W3C `traceparent` / `tracestate` from
///   the observability scope when present (pyfly's httpx adapter);
/// * retries on network errors and 429 / 5xx statuses with exponential
///   backoff (100 ms doubling, capped at 2 s), re-sending the full
///   JSON body on every attempt;
/// * decodes RFC 7807 `application/problem+json` error bodies into a
///   typed [`FireflyError`] carried by [`ClientError::Problem`].
///
/// > Porting note: the Go implementation creates the `bytes.Reader`
/// > for the encoded body once, outside its retry loop, so the first
/// > attempt exhausts it and every retried request goes out with
/// > `ContentLength: 0` and an empty body — a bodied retry can never
/// > succeed (the server's JSON decode fails). No Go test exercises a
/// > bodied retry. The Rust port implements the documented contract
/// > instead and re-sends the encoded body on every attempt.
#[derive(Debug, Clone)]
pub struct RestClient {
    base: String,
    headers: HeaderMap,
    http: reqwest::Client,
    retry_max: usize,
    backoff_base: Duration,
}

impl RestClient {
    /// Sends a request and JSON-decodes the 2xx response body into `T`.
    ///
    /// An empty 2xx body decodes as JSON `null`, so `T = ()` and
    /// `T = Option<_>` work for endpoints that return `204 No Content`.
    ///
    /// # Errors
    ///
    /// See [`ClientError`]; non-2xx responses surface as
    /// [`ClientError::Problem`] carrying the decoded [`FireflyError`].
    ///
    /// # Example
    ///
    /// ```no_run
    /// # async fn demo() -> Result<(), firefly_client::ClientError> {
    /// use firefly_client::RestBuilder;
    /// use http::Method;
    /// use serde::{Deserialize, Serialize};
    ///
    /// #[derive(Serialize)]
    /// struct CreateUser {
    ///     name: String,
    /// }
    /// #[derive(Deserialize)]
    /// struct User {
    ///     id: String,
    ///     name: String,
    /// }
    ///
    /// let client = RestBuilder::new("https://api.example.com").build();
    /// let user: User = client
    ///     .request(Method::POST, "/users", Some(&CreateUser { name: "alice".into() }))
    ///     .await?;
    /// assert_eq!(user.name, "alice");
    /// # let _ = user.id;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn request<B, T>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<T, ClientError>
    where
        B: Serialize + ?Sized,
        T: DeserializeOwned,
    {
        let raw = self.send(method, path, body).await?;
        if raw.is_empty() {
            serde_json::from_str("null").map_err(ClientError::Decode)
        } else {
            serde_json::from_slice(&raw).map_err(ClientError::Decode)
        }
    }

    /// Sends a request and returns the raw 2xx response body — the
    /// analog of calling Go's `Do` with `out == nil`, except the bytes
    /// are handed back instead of discarded.
    ///
    /// # Errors
    ///
    /// See [`ClientError`]; non-2xx responses surface as
    /// [`ClientError::Problem`] carrying the decoded [`FireflyError`].
    pub async fn send<B>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<Vec<u8>, ClientError>
    where
        B: Serialize + ?Sized,
    {
        let body_bytes = match body {
            Some(b) => Some(serde_json::to_vec(b).map_err(ClientError::Encode)?),
            None => None,
        };
        let raw_url = format!("{}{}", self.base, path);
        let url = reqwest::Url::parse(&raw_url)
            .map_err(|e| ClientError::InvalidUrl(format!("{raw_url}: {e}")))?;

        let mut last_err: Option<ClientError> = None;
        for attempt in 0..self.retry_max {
            let mut headers = self.headers.clone();
            if body_bytes.is_some() {
                headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            }
            headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
            if let Some(id) = correlation_id() {
                if let (Ok(name), Ok(value)) = (
                    HeaderName::from_bytes(HEADER_CORRELATION_ID.as_bytes()),
                    HeaderValue::from_str(&id),
                ) {
                    headers.insert(name, value);
                }
            }
            // pyfly's httpx adapter injects W3C `traceparent` (and
            // `tracestate`) on every outbound request when a trace
            // context is in scope; a no-op otherwise. Set alongside the
            // correlation id so the distributed trace stays unbroken.
            inject_headers(&mut headers);

            let mut req = self
                .http
                .request(method.clone(), url.clone())
                .headers(headers);
            // Deliberate divergence from the Go port: the full encoded
            // body is re-sent on every attempt. Go's reader is created
            // once outside its loop, so its retries go out bodyless —
            // see the porting note on [`RestClient`].
            if let Some(bytes) = &body_bytes {
                req = req.body(bytes.clone());
            }

            let resp = match req.send().await {
                Ok(resp) => resp,
                Err(e) => {
                    // Network errors always retry, as in the Go port.
                    last_err = Some(ClientError::Transport(e));
                    tokio::time::sleep(self.backoff(attempt)).await;
                    continue;
                }
            };

            let status = resp.status();
            let content_type = resp
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            // Read errors on the body are ignored, as in the Go port.
            let raw = resp.bytes().await.map(|b| b.to_vec()).unwrap_or_default();

            if status.is_success() {
                return Ok(raw);
            }

            // Decode RFC 7807 if available, otherwise wrap the raw body.
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
            if is_retryable_status(status.as_u16()) && attempt < self.retry_max - 1 {
                last_err = Some(ClientError::Problem(ferr));
                tokio::time::sleep(self.backoff(attempt)).await;
                continue;
            }
            return Err(ClientError::Problem(ferr));
        }
        Err(last_err.unwrap_or(ClientError::Exhausted(self.retry_max)))
    }

    /// Exponential backoff: `base << attempt`, capped at 2 s — the Go
    /// port's `backoff(attempt)` with a configurable base.
    fn backoff(&self, attempt: usize) -> Duration {
        let factor = 1u32
            .checked_shl(u32::try_from(attempt).unwrap_or(u32::MAX))
            .unwrap_or(u32::MAX);
        self.backoff_base.saturating_mul(factor).min(BACKOFF_CAP)
    }
}

/// Reports whether the status code is worth a retry: 429 Too Many
/// Requests or any 5xx — identical to the Go port.
fn is_retryable_status(code: u16) -> bool {
    code == 429 || code >= 500
}
