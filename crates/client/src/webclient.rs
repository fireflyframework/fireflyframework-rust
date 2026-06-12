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

//! Reactive HTTP client — the Spring `WebClient` analog.
//!
//! Where [`RestClient`](crate::RestClient) is the eager,
//! `async fn`-returning client (the Go-parity `Do(ctx, …)` surface),
//! [`WebClient`] is its **reactive** sibling: a fluent request builder
//! whose terminal operators hand back [`Mono<T>`] / [`Flux<T>`] from
//! [`firefly-reactive`](firefly_reactive) instead of bare futures. It is
//! the Rust spelling of Spring WebFlux's `WebClient`:
//!
//! ```text
//! client.method(GET).uri("/orders").header("X-Tenant", "acme")
//!       .query("page", "1").body(&req).retrieve()
//!       .body_to_mono::<Order>()        //  Mono<Order>
//! ```
//!
//! The streaming terminal, [`ResponseSpec::body_to_flux`], decodes a
//! chunked `application/x-ndjson` **or** `text/event-stream` response
//! *lazily*: the `reqwest` byte stream is split on newlines / SSE frames
//! and each element is deserialized as it arrives, with backpressure (a
//! `Flux` only pulls the next chunk when the downstream asks). This is
//! the analog of WebFlux's `responseSpec.bodyToFlux(Order.class)` over a
//! streaming media type.
//!
//! Everything the eager client does automatically — correlation-id and
//! W3C trace propagation, RFC 7807 `application/problem+json` decode into
//! a typed [`FireflyError`] — is preserved here, reusing the exact same
//! logic so the two surfaces never drift.

use std::time::Duration;

use bytes::{Bytes, BytesMut};
use futures::StreamExt;
use http::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, CONTENT_TYPE};
use http::Method;
use serde::de::DeserializeOwned;
use serde::Serialize;

use firefly_kernel::{
    correlation_id, FireflyError, ProblemDetail, HEADER_CORRELATION_ID, PROBLEM_CONTENT_TYPE,
};
use firefly_observability::inject_headers;
use firefly_reactive::{Flux, Mono};

use crate::error::ClientError;

/// Default per-request timeout (10 s), matching [`RestClient`](crate::RestClient).
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// The streaming NDJSON media type (`application/x-ndjson`), the same
/// type [`firefly-web`](https://docs.rs/firefly-web)'s `Flux` responder
/// emits. WebFlux's default streaming JSON content type.
pub const NDJSON_CONTENT_TYPE: &str = "application/x-ndjson";

/// The Server-Sent Events media type (`text/event-stream`). WebFlux maps
/// a `Flux<ServerSentEvent>` (or any `Flux` to an SSE endpoint) onto it.
pub const SSE_CONTENT_TYPE: &str = "text/event-stream";

/// Returns a [`WebClientBuilder`] primed for the given base URL — the
/// reactive analog of [`new_rest`](crate::new_rest), and the Rust
/// spelling of Spring's `WebClient.builder().baseUrl(url)`.
pub fn new_web_client(base_url: impl AsRef<str>) -> WebClientBuilder {
    WebClientBuilder::new(base_url)
}

/// Fluently configures a [`WebClient`].
///
/// Mirrors [`RestBuilder`](crate::RestBuilder): base URL (trailing
/// slashes trimmed), default headers, per-request timeout (default 10 s),
/// and an optional injected [`reqwest::Client`]. The reactive analog of
/// Spring's `WebClient.Builder`.
///
/// Unlike [`RestBuilder`](crate::RestBuilder) there is no retry budget:
/// retries on a reactive pipeline are expressed compositionally with
/// [`Mono::retry`](firefly_reactive::Mono::retry) /
/// [`Mono::retry_backoff`](firefly_reactive::Mono::retry_backoff) on the
/// returned `Mono`, exactly as WebFlux composes `retryWhen(..)` onto the
/// publisher rather than baking it into the client.
#[derive(Debug, Clone)]
pub struct WebClientBuilder {
    base_url: String,
    headers: HeaderMap,
    timeout: Duration,
    http_client: Option<reqwest::Client>,
}

impl WebClientBuilder {
    /// Returns a builder primed for the given base URL. Trailing `/`
    /// characters are trimmed so `base + path` concatenation stays clean
    /// (and an absolute `uri` passed to [`RequestSpec::uri`] still wins).
    pub fn new(base_url: impl AsRef<str>) -> Self {
        Self {
            base_url: base_url.as_ref().trim_end_matches('/').to_owned(),
            headers: HeaderMap::new(),
            timeout: DEFAULT_TIMEOUT,
            http_client: None,
        }
    }

    /// Sets a default request header sent on every request, replacing any
    /// previous value for the same name. Spring's
    /// `WebClient.Builder.defaultHeader(name, value)`.
    ///
    /// # Panics
    ///
    /// Panics when `key` is not a valid HTTP header name or `value` is
    /// not a valid header value — a programming error at wiring time,
    /// matching [`RestBuilder::with_header`](crate::RestBuilder::with_header).
    #[must_use]
    pub fn with_header(mut self, key: impl AsRef<str>, value: impl AsRef<str>) -> Self {
        let name = HeaderName::from_bytes(key.as_ref().as_bytes())
            .expect("WebClientBuilder::with_header: invalid header name");
        let value = HeaderValue::from_str(value.as_ref())
            .expect("WebClientBuilder::with_header: invalid header value");
        self.headers.insert(name, value);
        self
    }

    /// Overrides the per-request timeout (default 10 s). Ignored when a
    /// custom client is injected via
    /// [`with_http_client`](WebClientBuilder::with_http_client), whose own
    /// timeout configuration wins — the same contract as
    /// [`RestBuilder`](crate::RestBuilder).
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Injects a custom [`reqwest::Client`], used as-is. Spring's
    /// `WebClient.Builder.clientConnector(..)`.
    #[must_use]
    pub fn with_http_client(mut self, client: reqwest::Client) -> Self {
        self.http_client = Some(client);
        self
    }

    /// Finalises the client. When no custom client was injected, a
    /// [`reqwest::Client`] is built with the configured timeout.
    ///
    /// # Panics
    ///
    /// Panics if the underlying `reqwest` client cannot be constructed —
    /// a wiring-time error, matching [`RestBuilder::build`](crate::RestBuilder::build).
    pub fn build(self) -> WebClient {
        let http = self.http_client.unwrap_or_else(|| {
            reqwest::Client::builder()
                .timeout(self.timeout)
                .build()
                .expect("WebClientBuilder::build: reqwest client construction failed")
        });
        WebClient {
            base: self.base_url,
            headers: self.headers,
            http,
        }
    }
}

/// A reactive JSON-over-HTTP client built by [`WebClientBuilder`] — the
/// Rust analog of Spring WebFlux's `WebClient`.
///
/// Start a request with [`method`](WebClient::method) (or the
/// [`get`](WebClient::get) / [`post`](WebClient::post) /
/// [`put`](WebClient::put) / [`delete`](WebClient::delete) shorthands),
/// configure it on the returned [`RequestSpec`], then call
/// [`retrieve`](RequestSpec::retrieve) to obtain a [`ResponseSpec`] whose
/// terminal operators yield [`Mono`] / [`Flux`].
///
/// Every request, like [`RestClient`](crate::RestClient), automatically:
///
/// * sets `Accept: application/json` (overridable per request);
/// * JSON-encodes the body when present and sets
///   `Content-Type: application/json`;
/// * forwards the correlation id from the kernel task-local scope as
///   `X-Correlation-Id`, plus the W3C `traceparent` / `tracestate` from
///   the observability scope when present;
/// * decodes RFC 7807 `application/problem+json` error bodies into a
///   typed [`FireflyError`] carried by [`ClientError::Problem`] (the
///   `Mono` / `Flux` then signal that as their terminal `Err`).
#[derive(Debug, Clone)]
pub struct WebClient {
    base: String,
    headers: HeaderMap,
    http: reqwest::Client,
}

impl WebClient {
    /// Begins a request with the given HTTP [`Method`]. Spring's
    /// `webClient.method(HttpMethod.GET)`.
    pub fn method(&self, method: Method) -> RequestSpec {
        RequestSpec {
            http: self.http.clone(),
            base: self.base.clone(),
            headers: self.headers.clone(),
            method,
            path: String::new(),
            query: Vec::new(),
            body: None,
            encode_error: None,
        }
    }

    /// Shorthand for `method(Method::GET)`. Spring's `webClient.get()`.
    pub fn get(&self) -> RequestSpec {
        self.method(Method::GET)
    }

    /// Shorthand for `method(Method::POST)`. Spring's `webClient.post()`.
    pub fn post(&self) -> RequestSpec {
        self.method(Method::POST)
    }

    /// Shorthand for `method(Method::PUT)`. Spring's `webClient.put()`.
    pub fn put(&self) -> RequestSpec {
        self.method(Method::PUT)
    }

    /// Shorthand for `method(Method::DELETE)`. Spring's
    /// `webClient.delete()`.
    pub fn delete(&self) -> RequestSpec {
        self.method(Method::DELETE)
    }

    /// Shorthand for `method(Method::PATCH)`. Spring's `webClient.patch()`.
    pub fn patch(&self) -> RequestSpec {
        self.method(Method::PATCH)
    }
}

/// A request being built fluently — the analog of Spring's
/// `WebClient.RequestBodyUriSpec`.
///
/// Configure the target path with [`uri`](RequestSpec::uri), add
/// [`header`](RequestSpec::header) / [`query`](RequestSpec::query) pairs,
/// optionally set a JSON [`body`](RequestSpec::body), then call
/// [`retrieve`](RequestSpec::retrieve) to obtain a [`ResponseSpec`].
///
/// A body-encoding failure is captured here and surfaced lazily as the
/// terminal `Err` of the eventual `Mono` / `Flux`, so the fluent chain
/// never has to thread a `Result` — matching the reactive contract where
/// every failure is a signal on the publisher.
pub struct RequestSpec {
    http: reqwest::Client,
    base: String,
    headers: HeaderMap,
    method: Method,
    path: String,
    query: Vec<(String, String)>,
    body: Option<Vec<u8>>,
    encode_error: Option<serde_json::Error>,
}

impl RequestSpec {
    /// Sets the request URI. A value starting with `http://` or
    /// `https://` is used as an absolute URL; otherwise it is appended to
    /// the client's base URL. Spring's `requestSpec.uri(path)`.
    #[must_use]
    pub fn uri(mut self, uri: impl AsRef<str>) -> Self {
        self.path = uri.as_ref().to_owned();
        self
    }

    /// Adds a request header, replacing any previous value for the same
    /// name. Spring's `requestSpec.header(name, value)`.
    ///
    /// # Panics
    ///
    /// Panics when `key` is not a valid HTTP header name or `value` is
    /// not a valid header value.
    #[must_use]
    pub fn header(mut self, key: impl AsRef<str>, value: impl AsRef<str>) -> Self {
        let name = HeaderName::from_bytes(key.as_ref().as_bytes())
            .expect("RequestSpec::header: invalid header name");
        let value = HeaderValue::from_str(value.as_ref())
            .expect("RequestSpec::header: invalid header value");
        self.headers.insert(name, value);
        self
    }

    /// Appends a `key=value` query parameter (values are URL-encoded by
    /// `reqwest`). Repeated calls add repeated parameters. The analog of
    /// building a `UriComponentsBuilder.queryParam(..)` in WebFlux.
    #[must_use]
    pub fn query(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.query.push((key.into(), value.into()));
        self
    }

    /// Sets the JSON request body, encoding `value` immediately. A
    /// serialization failure is captured and re-emitted as the terminal
    /// error of the eventual `Mono` / `Flux`. Spring's
    /// `requestSpec.bodyValue(value)`.
    #[must_use]
    pub fn body<B>(mut self, value: &B) -> Self
    where
        B: Serialize + ?Sized,
    {
        match serde_json::to_vec(value) {
            Ok(bytes) => self.body = Some(bytes),
            Err(e) => self.encode_error = Some(e),
        }
        self
    }

    /// Finalises the request configuration into a [`ResponseSpec`], the
    /// gateway to the reactive body decoders. Spring's
    /// `requestSpec.retrieve()`. No I/O happens yet — the request is sent
    /// lazily when the returned `Mono` / `Flux` is subscribed.
    pub fn retrieve(self) -> ResponseSpec {
        ResponseSpec { request: self }
    }

    /// Resolves the absolute request URL from the base + path.
    fn resolve_url(&self) -> Result<reqwest::Url, ClientError> {
        let raw = if self.path.starts_with("http://") || self.path.starts_with("https://") {
            self.path.clone()
        } else {
            format!("{}{}", self.base, self.path)
        };
        reqwest::Url::parse(&raw).map_err(|e| ClientError::InvalidUrl(format!("{raw}: {e}")))
    }

    /// Builds the per-request headers: defaults + `Accept` +
    /// `Content-Type` (when bodied) + correlation/trace propagation.
    /// Reuses the exact propagation logic of
    /// [`RestClient`](crate::RestClient) so the two surfaces never drift.
    fn build_headers(&self) -> HeaderMap {
        let mut headers = self.headers.clone();
        if self.body.is_some() {
            headers
                .entry(CONTENT_TYPE)
                .or_insert_with(|| HeaderValue::from_static("application/json"));
        }
        headers
            .entry(ACCEPT)
            .or_insert_with(|| HeaderValue::from_static("application/json"));
        if let Some(id) = correlation_id() {
            if let (Ok(name), Ok(value)) = (
                HeaderName::from_bytes(HEADER_CORRELATION_ID.as_bytes()),
                HeaderValue::from_str(&id),
            ) {
                headers.insert(name, value);
            }
        }
        // pyfly's httpx adapter injects W3C `traceparent` / `tracestate`
        // when a trace context is in scope; a no-op otherwise.
        inject_headers(&mut headers);
        headers
    }

    /// Sends the request and returns the raw `reqwest::Response`, after
    /// converting a non-2xx status into a decoded [`ClientError`].
    async fn send(self) -> Result<reqwest::Response, ClientError> {
        if let Some(e) = self.encode_error {
            return Err(ClientError::Encode(e));
        }
        let url = self.resolve_url()?;
        let headers = self.build_headers();

        let mut req = self.http.request(self.method, url).headers(headers);
        if !self.query.is_empty() {
            req = req.query(&self.query);
        }
        if let Some(bytes) = self.body {
            req = req.body(bytes);
        }

        let resp = req.send().await.map_err(ClientError::Transport)?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }

        // Non-2xx: decode the (possibly RFC 7807) error body, mirroring
        // RestClient::send. We must read the body here, so it is fully
        // buffered for the error path only.
        let content_type = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let raw = resp.bytes().await.map(|b| b.to_vec()).unwrap_or_default();
        Err(ClientError::Problem(decode_problem(
            status.as_u16(),
            status.canonical_reason().unwrap_or_default(),
            &content_type,
            &raw,
        )))
    }
}

/// The gateway to the reactive body decoders — the analog of Spring's
/// `WebClient.ResponseSpec`.
///
/// Choose a terminal operator:
///
/// * [`body_to_mono`](ResponseSpec::body_to_mono) — decode the whole 2xx
///   body as a single `T` into a [`Mono<T>`];
/// * [`body_to_flux`](ResponseSpec::body_to_flux) — stream a chunked
///   NDJSON / SSE body element-by-element into a [`Flux<T>`], lazily and
///   with backpressure;
/// * [`exchange`](ResponseSpec::exchange) — get the raw reactive
///   [`WebClientResponse`] (status + headers + buffered body) without
///   committing to a decode.
///
/// Each operator is lazy: no request is sent until the returned `Mono` /
/// `Flux` is subscribed (`block`ed, `await`ed, or streamed).
pub struct ResponseSpec {
    request: RequestSpec,
}

impl ResponseSpec {
    /// Decodes the entire 2xx response body as a single value into a
    /// [`Mono<T>`]. An empty body decodes as JSON `null`, so `T = ()` and
    /// `T = Option<_>` model a `204 No Content`. Spring's
    /// `responseSpec.bodyToMono(T.class)`.
    ///
    /// A non-2xx response, a transport failure, or a JSON decode error
    /// becomes the `Mono`'s terminal `Err` (a [`FireflyError`]).
    ///
    /// # Example
    ///
    /// ```no_run
    /// # async fn demo() {
    /// use firefly_client::WebClientBuilder;
    /// use http::Method;
    /// use serde::Deserialize;
    ///
    /// #[derive(Deserialize)]
    /// struct Order { id: String }
    ///
    /// let client = WebClientBuilder::new("https://api.example.com").build();
    /// let order = client
    ///     .method(Method::GET)
    ///     .uri("/orders/1")
    ///     .retrieve()
    ///     .body_to_mono::<Order>()
    ///     .block()
    ///     .await
    ///     .unwrap();
    /// # let _ = order.map(|o| o.id);
    /// # }
    /// ```
    pub fn body_to_mono<T>(self) -> Mono<T>
    where
        T: DeserializeOwned + Send + 'static,
    {
        Mono::from_result_future(async move {
            let resp = self.request.send().await.map_err(into_firefly)?;
            let raw = resp
                .bytes()
                .await
                .map_err(|e| into_firefly(ClientError::Transport(e)))?;
            if raw.is_empty() {
                serde_json::from_str("null")
            } else {
                serde_json::from_slice(&raw)
            }
            .map_err(|e| into_firefly(ClientError::Decode(e)))
        })
    }

    /// Streams a chunked `application/x-ndjson` **or** `text/event-stream`
    /// 2xx response body into a [`Flux<T>`], decoding one `T` per line
    /// (NDJSON) or per SSE `data:` frame — **lazily and with
    /// backpressure**. Spring's `responseSpec.bodyToFlux(T.class)` over a
    /// streaming media type.
    ///
    /// The decoder is chosen from the response `Content-Type`: a
    /// `text/event-stream` body is parsed as SSE (frames separated by a
    /// blank line; `data:` payloads concatenated; comment / `event:` /
    /// `id:` lines ignored); anything else is treated as NDJSON (one JSON
    /// document per non-empty line). The underlying `reqwest` byte stream
    /// is consumed chunk-by-chunk and only advances as the downstream
    /// pulls, so a slow consumer naturally throttles the producer.
    ///
    /// A non-2xx response or a transport failure becomes the `Flux`'s
    /// terminal `Err`; a malformed element surfaces as a decode
    /// [`FireflyError`] that terminates the stream (Reactor's
    /// first-error-is-terminal semantics).
    ///
    /// # Example
    ///
    /// ```no_run
    /// # async fn demo() {
    /// use firefly_client::WebClientBuilder;
    /// use http::Method;
    /// use serde::Deserialize;
    ///
    /// #[derive(Deserialize)]
    /// struct Tick { seq: u64 }
    ///
    /// let client = WebClientBuilder::new("https://api.example.com").build();
    /// let ticks = client
    ///     .method(Method::GET)
    ///     .uri("/ticks")
    ///     .retrieve()
    ///     .body_to_flux::<Tick>()
    ///     .collect_list()
    ///     .block()
    ///     .await
    ///     .unwrap();
    /// # let _ = ticks;
    /// # }
    /// ```
    pub fn body_to_flux<T>(self) -> Flux<T>
    where
        T: DeserializeOwned + Send + 'static,
    {
        Flux::from_stream(async_stream::try_stream! {
            let resp = self.request.send().await.map_err(into_firefly)?;
            let is_sse = resp
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(|ct| ct.starts_with(SSE_CONTENT_TYPE))
                .unwrap_or(false);

            let mut byte_stream = resp.bytes_stream();
            let mut decoder = FrameDecoder::new(is_sse);
            while let Some(chunk) = byte_stream.next().await {
                let chunk = chunk.map_err(|e| into_firefly(ClientError::Transport(e)))?;
                decoder.push(&chunk);
                while let Some(payload) = decoder.next_frame() {
                    let value: T = serde_json::from_slice(&payload)
                        .map_err(|e| into_firefly(ClientError::Decode(e)))?;
                    yield value;
                }
            }
            // Flush a trailing frame with no terminating newline / blank
            // line (a well-formed NDJSON or SSE stream usually ends with
            // one, but we are lenient).
            if let Some(payload) = decoder.flush() {
                let value: T = serde_json::from_slice(&payload)
                    .map_err(|e| into_firefly(ClientError::Decode(e)))?;
                yield value;
            }
        })
    }

    /// Returns the raw reactive response — status, headers, and the fully
    /// buffered body — as a [`Mono<WebClientResponse>`], **without**
    /// raising on a non-2xx status. Spring's `requestSpec.exchange()` /
    /// `exchangeToMono(..)`: the caller inspects
    /// [`status`](WebClientResponse::status) and decides what to do.
    ///
    /// Use this for body-less status / header checks, or when a non-2xx
    /// response is expected and should not short-circuit the pipeline.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # async fn demo() {
    /// use firefly_client::WebClientBuilder;
    /// use http::Method;
    ///
    /// let client = WebClientBuilder::new("https://api.example.com").build();
    /// let resp = client
    ///     .method(Method::GET)
    ///     .uri("/health")
    ///     .retrieve()
    ///     .exchange()
    ///     .block()
    ///     .await
    ///     .unwrap()
    ///     .unwrap();
    /// assert!(resp.is_success());
    /// # }
    /// ```
    pub fn exchange(self) -> Mono<WebClientResponse> {
        Mono::from_result_future(async move {
            let req = self.request;
            if let Some(e) = req.encode_error {
                return Err(into_firefly(ClientError::Encode(e)));
            }
            let url = req.resolve_url().map_err(into_firefly)?;
            let headers = req.build_headers();
            let mut builder = req.http.request(req.method, url).headers(headers);
            if !req.query.is_empty() {
                builder = builder.query(&req.query);
            }
            if let Some(bytes) = req.body {
                builder = builder.body(bytes);
            }
            let resp = builder
                .send()
                .await
                .map_err(|e| into_firefly(ClientError::Transport(e)))?;
            let status = resp.status().as_u16();
            let headers = resp.headers().clone();
            let body = resp
                .bytes()
                .await
                .map_err(|e| into_firefly(ClientError::Transport(e)))?;
            Ok(WebClientResponse {
                status,
                headers,
                body,
            })
        })
    }
}

/// The raw reactive response returned by [`ResponseSpec::exchange`] — the
/// analog of Spring's `ClientResponse`.
///
/// Carries the HTTP status, the response headers, and the fully buffered
/// body. Decode the body with [`body_json`](WebClientResponse::body_json)
/// once you have inspected the status, or read [`body`](WebClientResponse::body)
/// bytes directly.
#[derive(Debug, Clone)]
pub struct WebClientResponse {
    status: u16,
    headers: HeaderMap,
    body: Bytes,
}

impl WebClientResponse {
    /// The HTTP status code. Spring's `clientResponse.statusCode()`.
    pub fn status(&self) -> u16 {
        self.status
    }

    /// Whether the status is in the 2xx range. Spring's
    /// `clientResponse.statusCode().is2xxSuccessful()`.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// The response headers. Spring's `clientResponse.headers()`.
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// The raw response body bytes.
    pub fn body(&self) -> &Bytes {
        &self.body
    }

    /// JSON-decodes the buffered body into `T`. An empty body decodes as
    /// JSON `null`. The analog of `clientResponse.bodyToMono(T.class)`
    /// once you already hold the materialized response.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Decode`] when the body is not valid JSON for
    /// `T`.
    pub fn body_json<T>(&self) -> Result<T, ClientError>
    where
        T: DeserializeOwned,
    {
        if self.body.is_empty() {
            serde_json::from_str("null").map_err(ClientError::Decode)
        } else {
            serde_json::from_slice(&self.body).map_err(ClientError::Decode)
        }
    }

    /// Decodes the body as an RFC 7807 problem into a [`FireflyError`]
    /// when the response is a non-2xx `application/problem+json` (falling
    /// back to status + raw body otherwise). Returns `None` for a 2xx
    /// response. The reactive-`exchange` analog of the automatic decode
    /// [`body_to_mono`](ResponseSpec::body_to_mono) performs.
    pub fn problem(&self) -> Option<FireflyError> {
        if self.is_success() {
            return None;
        }
        let content_type = self
            .headers
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        let reason = http::StatusCode::from_u16(self.status)
            .ok()
            .and_then(|s| s.canonical_reason())
            .unwrap_or_default();
        Some(decode_problem(
            self.status,
            reason,
            content_type,
            &self.body,
        ))
    }
}

/// Incremental NDJSON / SSE frame decoder.
///
/// Fed raw byte chunks via [`push`](FrameDecoder::push); yields one
/// decoded *payload* (the bytes to deserialize) per
/// [`next_frame`](FrameDecoder::next_frame) call. In NDJSON mode a frame
/// is a single non-empty line; in SSE mode a frame is the concatenation
/// of the `data:` lines of one event block (terminated by a blank line).
struct FrameDecoder {
    buf: BytesMut,
    sse: bool,
}

impl FrameDecoder {
    fn new(sse: bool) -> Self {
        Self {
            buf: BytesMut::new(),
            sse,
        }
    }

    /// Appends a raw byte chunk to the internal buffer.
    fn push(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    /// Pops the next complete frame's payload, or `None` if the buffer
    /// does not yet hold a full frame.
    fn next_frame(&mut self) -> Option<Vec<u8>> {
        if self.sse {
            self.next_sse_frame()
        } else {
            self.next_ndjson_frame()
        }
    }

    /// NDJSON: one JSON document per `\n`-terminated line. Empty lines are
    /// skipped.
    fn next_ndjson_frame(&mut self) -> Option<Vec<u8>> {
        loop {
            let nl = self.buf.iter().position(|&b| b == b'\n')?;
            let line = self.buf.split_to(nl + 1);
            let trimmed = trim_ascii(&line[..line.len() - 1]);
            if trimmed.is_empty() {
                continue;
            }
            return Some(trimmed.to_vec());
        }
    }

    /// SSE: an event block ends at a blank line (`\n\n`). Within a block,
    /// `data:` lines are concatenated with `\n`; `event:` / `id:` /
    /// comment (`:`) lines and unknown fields are ignored. A block with no
    /// `data` is skipped (e.g. a keep-alive comment).
    fn next_sse_frame(&mut self) -> Option<Vec<u8>> {
        loop {
            let sep = find_blank_line(&self.buf)?;
            let block = self.buf.split_to(sep.end);
            let block = &block[..sep.start];
            let payload = parse_sse_block(block);
            if let Some(payload) = payload {
                return Some(payload);
            }
            // No `data:` in this block (comment / keep-alive) — keep going.
        }
    }

    /// Returns a final unterminated frame at end-of-stream, if any.
    fn flush(&mut self) -> Option<Vec<u8>> {
        if self.sse {
            let block = std::mem::take(&mut self.buf);
            parse_sse_block(&block)
        } else {
            let line = std::mem::take(&mut self.buf);
            let trimmed = trim_ascii(&line);
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_vec())
            }
        }
    }
}

/// Concatenates the `data:` values of one SSE event block, or `None` when
/// the block carries no `data` field.
fn parse_sse_block(block: &[u8]) -> Option<Vec<u8>> {
    let mut data: Vec<u8> = Vec::new();
    let mut saw_data = false;
    for line in block.split(|&b| b == b'\n') {
        let line = strip_cr(line);
        if line.is_empty() || line.first() == Some(&b':') {
            continue; // blank line within block, or a comment
        }
        if let Some(rest) = line.strip_prefix(b"data:") {
            if saw_data {
                data.push(b'\n');
            }
            saw_data = true;
            // A single leading space after the colon is stripped per the
            // SSE spec.
            let rest = rest.strip_prefix(b" ").unwrap_or(rest);
            data.extend_from_slice(rest);
        }
        // `event:` / `id:` / `retry:` / unknown fields are ignored.
    }
    if saw_data {
        Some(data)
    } else {
        None
    }
}

/// The half-open byte range `[start, end)` of a blank-line separator: the
/// `start` is where the preceding block ends and `end` is just past the
/// separator (so the separator bytes are consumed). Handles both `\n\n`
/// and `\r\n\r\n`.
struct BlankLine {
    start: usize,
    end: usize,
}

/// Finds the first blank-line separator (`\n\n` or `\r\n\r\n`) in `buf`.
fn find_blank_line(buf: &[u8]) -> Option<BlankLine> {
    for i in 0..buf.len() {
        if buf[i] == b'\n' {
            // `\n\n`
            if buf.get(i + 1) == Some(&b'\n') {
                return Some(BlankLine {
                    start: i,
                    end: i + 2,
                });
            }
            // `\n\r\n`
            if buf.get(i + 1) == Some(&b'\r') && buf.get(i + 2) == Some(&b'\n') {
                return Some(BlankLine {
                    start: i,
                    end: i + 3,
                });
            }
        }
    }
    None
}

/// Strips a single trailing `\r` (for `\r\n` line endings).
fn strip_cr(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// Trims leading and trailing ASCII whitespace.
fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes.iter().position(|b| !b.is_ascii_whitespace());
    let Some(start) = start else { return &[] };
    let end = bytes
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .unwrap_or(start);
    &bytes[start..=end]
}

/// Decodes a non-2xx response into a [`FireflyError`], preferring an RFC
/// 7807 `application/problem+json` body. The shared core of
/// [`RestClient`](crate::RestClient)'s error path, reused verbatim so the
/// two surfaces decode identically.
fn decode_problem(status: u16, reason: &str, content_type: &str, raw: &[u8]) -> FireflyError {
    let mut ferr = FireflyError::new(
        String::new(),
        reason,
        status,
        String::from_utf8_lossy(raw).into_owned(),
    );
    if content_type.starts_with(PROBLEM_CONTENT_TYPE) {
        if let Ok(pd) = serde_json::from_slice::<ProblemDetail>(raw) {
            ferr.code = pd.problem_type;
            ferr.title = pd.title;
            ferr.status = pd.status;
            ferr.detail = pd.detail;
            ferr.fields = pd.extensions;
        }
    }
    ferr
}

/// Converts a [`ClientError`] into the [`FireflyError`] terminal signal a
/// reactive publisher carries. A [`ClientError::Problem`] passes its
/// inner [`FireflyError`] through untouched; every other variant is
/// wrapped (preserving its message and, where present, its source).
fn into_firefly(err: ClientError) -> FireflyError {
    match err {
        ClientError::Problem(fe) => fe,
        ClientError::Transport(e) => {
            FireflyError::new("CLIENT_TRANSPORT", "Bad Gateway", 502, e.to_string()).with_cause(e)
        }
        ClientError::InvalidUrl(s) => {
            FireflyError::new("CLIENT_INVALID_URL", "Bad Request", 400, s)
        }
        ClientError::Encode(e) => {
            FireflyError::new("CLIENT_ENCODE", "Internal Server Error", 500, e.to_string())
        }
        ClientError::Decode(e) => {
            FireflyError::new("CLIENT_DECODE", "Bad Gateway", 502, e.to_string())
        }
        ClientError::Exhausted(n) => FireflyError::new(
            "CLIENT_EXHAUSTED",
            "Service Unavailable",
            503,
            format!("exhausted {n} attempts"),
        ),
        ClientError::GraphQl(errs) => FireflyError::new(
            "CLIENT_GRAPHQL",
            "Bad Gateway",
            502,
            format!("graphql errors: {errs:?}"),
        ),
        ClientError::TransportNotRegistered => FireflyError::new(
            "CLIENT_TRANSPORT_NOT_REGISTERED",
            "Not Implemented",
            501,
            "transport adapter not registered",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ndjson_decoder_splits_lines_and_skips_blanks() {
        let mut d = FrameDecoder::new(false);
        d.push(b"{\"a\":1}\n\n{\"a\":2}\n");
        assert_eq!(d.next_frame().as_deref(), Some(&b"{\"a\":1}"[..]));
        assert_eq!(d.next_frame().as_deref(), Some(&b"{\"a\":2}"[..]));
        assert!(d.next_frame().is_none());
        assert!(d.flush().is_none());
    }

    #[test]
    fn ndjson_decoder_handles_split_chunks() {
        let mut d = FrameDecoder::new(false);
        d.push(b"{\"a\"");
        assert!(d.next_frame().is_none());
        d.push(b":1}\n");
        assert_eq!(d.next_frame().as_deref(), Some(&b"{\"a\":1}"[..]));
    }

    #[test]
    fn ndjson_decoder_flushes_unterminated_tail() {
        let mut d = FrameDecoder::new(false);
        d.push(b"{\"a\":1}");
        assert!(d.next_frame().is_none());
        assert_eq!(d.flush().as_deref(), Some(&b"{\"a\":1}"[..]));
    }

    #[test]
    fn sse_decoder_concatenates_data_and_skips_comments() {
        let mut d = FrameDecoder::new(true);
        d.push(b": keep-alive\n\n");
        d.push(b"event: tick\ndata: {\"n\":1}\n\n");
        d.push(b"data: {\"n\"\ndata: :2}\n\n");
        // The comment-only block is skipped.
        assert_eq!(d.next_frame().as_deref(), Some(&b"{\"n\":1}"[..]));
        assert_eq!(d.next_frame().as_deref(), Some(&b"{\"n\"\n:2}"[..]));
        assert!(d.next_frame().is_none());
    }

    #[test]
    fn sse_decoder_handles_crlf_line_endings() {
        let mut d = FrameDecoder::new(true);
        d.push(b"data: {\"n\":1}\r\n\r\n");
        assert_eq!(d.next_frame().as_deref(), Some(&b"{\"n\":1}"[..]));
    }

    #[test]
    fn problem_passthrough_preserves_firefly_error() {
        let fe = FireflyError::not_found("missing");
        let out = into_firefly(ClientError::Problem(fe));
        assert_eq!(out.status, 404);
        assert_eq!(out.detail, "missing");
    }

    #[test]
    fn decode_problem_reads_rfc7807_body() {
        let pd = ProblemDetail::bad_request("nope").with("field", "amount");
        let raw = serde_json::to_vec(&pd).expect("encode");
        let fe = decode_problem(400, "Bad Request", PROBLEM_CONTENT_TYPE, &raw);
        assert_eq!(fe.status, 400);
        assert_eq!(fe.detail, "nope");
        assert_eq!(fe.fields.get("field"), Some(&serde_json::json!("amount")));
    }
}
