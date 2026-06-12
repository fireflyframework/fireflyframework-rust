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

//! Typed forwarder over `POST /api/webhooks/{provider}` — the Rust
//! spelling of the Go `webhooks/sdk` package. Used to replay DLQ
//! entries and to compose webhook ingestion across services.
//!
//! [`Client::forward`] sends the payload as the **raw request body**
//! (the exact bytes the receiving validators sign over) together with
//! the caller-supplied headers, which must include the provider's
//! signature header (e.g. `Stripe-Signature`), since the framework's
//! validators run on the receiving end. Retries, backoff, correlation
//! propagation, and RFC 7807 error decoding behave exactly like
//! [`firefly_client::RestClient`].
//!
//! > Porting note: the Go implementation builds the headed request and
//! > then forwards through `RESTClient.Do`, which JSON-re-encodes the
//! > payload and drops the per-call headers — dead code its own
//! > documentation contradicts and no Go test exercises. The Rust port
//! > implements the documented contract (raw body + headers), without
//! > which no forwarded event could ever pass signature validation.

use std::time::Duration;

use http::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, CONTENT_TYPE};

use firefly_client::ClientError;
use firefly_kernel::{
    correlation_id, FireflyError, ProblemDetail, HEADER_CORRELATION_ID, PROBLEM_CONTENT_TYPE,
};

/// Default per-request timeout (10 s), matching the framework client.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
/// Default total attempt budget (3), matching the framework client.
const DEFAULT_RETRIES: usize = 3;
/// Default first-retry backoff delay (100 ms), doubling per attempt.
const DEFAULT_BACKOFF_BASE: Duration = Duration::from_millis(100);
/// Exponential backoff ceiling (2 s), matching the framework client.
const BACKOFF_CAP: Duration = Duration::from_secs(2);

/// The typed SDK over `POST /api/webhooks/{provider}` — Go's
/// `sdk.Client`.
///
/// # Example
///
/// ```no_run
/// # async fn demo() -> Result<(), firefly_client::ClientError> {
/// use firefly_webhooks::sdk::Client;
///
/// let client = Client::new("https://ingest.example.com");
/// client
///     .forward(
///         "github",
///         br#"{"action":"opened"}"#,
///         &[("X-Hub-Signature-256", "sha256=…")],
///     )
///     .await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct Client {
    base: String,
    http: reqwest::Client,
    retry_max: usize,
    backoff_base: Duration,
}

impl Client {
    /// Returns a client targeting `base_url` (trailing slashes
    /// trimmed) — Go's `sdk.New(baseURL)`. Defaults match the
    /// framework REST client: 10 s timeout, 3 attempts, 100 ms
    /// doubling backoff.
    pub fn new(base_url: impl AsRef<str>) -> Self {
        Self {
            base: base_url.as_ref().trim_end_matches('/').to_owned(),
            http: reqwest::Client::builder()
                .timeout(DEFAULT_TIMEOUT)
                .build()
                .expect("Client::new: reqwest client construction failed"),
            retry_max: DEFAULT_RETRIES,
            backoff_base: DEFAULT_BACKOFF_BASE,
        }
    }

    /// Overrides the total attempt budget (default 3).
    #[must_use]
    pub fn with_retries(mut self, attempts: usize) -> Self {
        self.retry_max = attempts;
        self
    }

    /// Overrides the first-retry backoff delay (default 100 ms; each
    /// retry doubles it, capped at 2 s).
    #[must_use]
    pub fn with_backoff_base(mut self, base: Duration) -> Self {
        self.backoff_base = base;
        self
    }

    /// Injects a custom [`reqwest::Client`], used as-is.
    #[must_use]
    pub fn with_http_client(mut self, client: reqwest::Client) -> Self {
        self.http = client;
        self
    }

    /// POSTs `payload` to `/api/webhooks/{provider}` with the given
    /// headers. Headers must include the provider's signature (e.g.
    /// `Stripe-Signature`), since the framework's validators run on
    /// the receiving end. `Content-Type` defaults to
    /// `application/json` but a caller-supplied value wins.
    ///
    /// # Errors
    ///
    /// The same [`ClientError`] family as the framework REST client:
    /// [`ClientError::Problem`] for a non-2xx response (RFC 7807
    /// decoded when present — a `404` means the receiving side has no
    /// validator for `provider`, a `401` that the signature header was
    /// rejected), [`ClientError::Transport`] for network failures, and
    /// [`ClientError::InvalidUrl`] when the URL does not parse.
    ///
    /// # Panics
    ///
    /// Panics when a header name or value is invalid — a programming
    /// error at the call site, exactly like
    /// [`firefly_client::RestBuilder::with_header`].
    pub async fn forward(
        &self,
        provider: &str,
        payload: &[u8],
        headers: &[(&str, &str)],
    ) -> Result<(), ClientError> {
        let raw_url = format!("{}/api/webhooks/{}", self.base, provider);
        let url = reqwest::Url::parse(&raw_url)
            .map_err(|e| ClientError::InvalidUrl(format!("{raw_url}: {e}")))?;

        let mut base_headers = HeaderMap::new();
        base_headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        for (key, value) in headers {
            let name = HeaderName::from_bytes(key.as_bytes())
                .expect("Client::forward: invalid header name");
            let value =
                HeaderValue::from_str(value).expect("Client::forward: invalid header value");
            base_headers.insert(name, value);
        }
        base_headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

        let mut last_err: Option<ClientError> = None;
        for attempt in 0..self.retry_max {
            let mut req_headers = base_headers.clone();
            if let Some(id) = correlation_id() {
                if let (Ok(name), Ok(value)) = (
                    HeaderName::from_bytes(HEADER_CORRELATION_ID.as_bytes()),
                    HeaderValue::from_str(&id),
                ) {
                    req_headers.insert(name, value);
                }
            }

            let resp = match self
                .http
                .post(url.clone())
                .headers(req_headers)
                .body(payload.to_vec())
                .send()
                .await
            {
                Ok(resp) => resp,
                Err(e) => {
                    // Network errors always retry, as in the Go client.
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
            let raw = resp.bytes().await.map(|b| b.to_vec()).unwrap_or_default();

            if status.is_success() {
                return Ok(());
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

    /// Exponential backoff: `base << attempt`, capped at 2 s.
    fn backoff(&self, attempt: usize) -> Duration {
        let factor = 1u32
            .checked_shl(u32::try_from(attempt).unwrap_or(u32::MAX))
            .unwrap_or(u32::MAX);
        self.backoff_base.saturating_mul(factor).min(BACKOFF_CAP)
    }
}

/// 429 Too Many Requests or any 5xx is worth a retry — identical to
/// the framework REST client.
fn is_retryable_status(code: u16) -> bool {
    code == 429 || code >= 500
}
