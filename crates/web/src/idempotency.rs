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

//! Idempotency-key replay middleware — the Rust port of the Go module's
//! `idempotency.go` (`IdempotencyMiddleware`, `IdempotencyStore`,
//! `MemoryIdempotencyStore`, `IdempotencyRecord`).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::convert::Infallible;
use std::sync::{Arc, LazyLock, PoisonError, RwLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::body::{to_bytes, Body};
use axum::response::Response;
use chrono::{DateTime, Utc};
use firefly_kernel::{FireflyError, FireflyResult, ProblemDetail, HEADER_IDEMPOTENCY_KEY};
use futures::future::BoxFuture;
use http::{HeaderName, HeaderValue, Method, Request, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tower::{Layer, Service};

use crate::problem::problem_response;

/// The `Idempotency-Key` header as a typed [`HeaderName`], derived from
/// the kernel constant so there is a single source of truth.
static IDEMPOTENCY_HEADER: LazyLock<HeaderName> = LazyLock::new(|| {
    HeaderName::from_bytes(HEADER_IDEMPOTENCY_KEY.as_bytes()).expect("valid header name")
});

/// The replay marker header set on responses served from the store.
static REPLAY_HEADER: LazyLock<HeaderName> =
    LazyLock::new(|| HeaderName::from_static("idempotent-replay"));

/// Captures the response replayed on a repeated request with the same
/// `Idempotency-Key`. The JSON shape (`status`, `headers`, `body` as
/// base64, `bodyHash`, `storedAt`, `requestHash`) matches the Go port's
/// `IdempotencyRecord` so records written by one runtime replay on
/// another when a shared store (Redis, Postgres, …) is plugged in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdempotencyRecord {
    /// HTTP status code of the original response (JSON `status`).
    #[serde(rename = "status")]
    pub status_code: u16,
    /// First value of each response header (JSON `headers`).
    pub headers: BTreeMap<String, String>,
    /// Raw response body; serialized as base64, matching Go's `[]byte`
    /// (JSON `body`).
    #[serde(with = "base64_bytes")]
    pub body: Vec<u8>,
    /// Hex-encoded SHA-256 of [`IdempotencyRecord::body`] (JSON `bodyHash`).
    #[serde(rename = "bodyHash")]
    pub body_hash: String,
    /// UTC instant the record was stored (JSON `storedAt`).
    #[serde(rename = "storedAt")]
    pub stored_at: DateTime<Utc>,
    /// Hex-encoded SHA-256 of the request body, used to detect key reuse
    /// with a different payload (JSON `requestHash`).
    #[serde(rename = "requestHash")]
    pub request_hash: String,
}

/// Serde adapter encoding `Vec<u8>` as a standard-alphabet base64 string
/// — the JSON encoding Go applies to `[]byte`.
mod base64_bytes {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use serde::de::Error as DeError;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let raw = String::deserialize(deserializer)?;
        STANDARD.decode(raw).map_err(DeError::custom)
    }
}

/// The pluggable persistence boundary used by [`IdempotencyLayer`].
/// Implementations must be safe for concurrent use. The in-process
/// default is [`MemoryIdempotencyStore`]; production deployments plug a
/// shared store (Redis / Postgres / …) behind this trait.
#[async_trait]
pub trait IdempotencyStore: Send + Sync {
    /// Returns the record stored under `key`, or `None` when absent or
    /// expired.
    async fn get(&self, key: &str) -> FireflyResult<Option<IdempotencyRecord>>;

    /// Stores `rec` under `key` for `ttl`; a zero `ttl` means the record
    /// never expires.
    async fn put(&self, key: &str, rec: IdempotencyRecord, ttl: Duration) -> FireflyResult<()>;
}

struct MemEntry {
    rec: IdempotencyRecord,
    exp: Option<Instant>,
}

/// The default in-process [`IdempotencyStore`] — suitable for tests and
/// single-instance deployments, mirroring the Go port's
/// `MemoryIdempotencyStore`.
#[derive(Default)]
pub struct MemoryIdempotencyStore {
    entries: RwLock<HashMap<String, MemEntry>>,
}

impl MemoryIdempotencyStore {
    /// Returns an empty in-memory store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl IdempotencyStore for MemoryIdempotencyStore {
    async fn get(&self, key: &str) -> FireflyResult<Option<IdempotencyRecord>> {
        let entries = self.entries.read().unwrap_or_else(PoisonError::into_inner);
        Ok(entries
            .get(key)
            .filter(|e| e.exp.is_none_or(|exp| Instant::now() <= exp))
            .map(|e| e.rec.clone()))
    }

    async fn put(&self, key: &str, rec: IdempotencyRecord, ttl: Duration) -> FireflyResult<()> {
        let exp = (ttl > Duration::ZERO).then(|| Instant::now() + ttl);
        self.entries
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.to_owned(), MemEntry { rec, exp });
        Ok(())
    }
}

/// Tunes [`IdempotencyLayer`]. The defaults — POST/PUT/PATCH, 24-hour
/// TTL, a memory store — match the Go `DefaultIdempotencyConfig()` and
/// the Java/.NET defaults.
#[derive(Clone)]
pub struct IdempotencyConfig {
    /// Where replay records are persisted.
    pub store: Arc<dyn IdempotencyStore>,
    /// How long records live; [`IdempotencyLayer::new`] normalizes a
    /// zero duration to 24 hours, mirroring the Go middleware.
    pub ttl: Duration,
    /// The HTTP methods subject to idempotency handling.
    pub methods: HashSet<Method>,
}

impl Default for IdempotencyConfig {
    /// The canonical config: memory store, 24-hour TTL, POST/PUT/PATCH.
    fn default() -> Self {
        Self {
            store: Arc::new(MemoryIdempotencyStore::new()),
            ttl: Duration::from_secs(24 * 60 * 60),
            methods: [Method::POST, Method::PUT, Method::PATCH]
                .into_iter()
                .collect(),
        }
    }
}

/// Replays the stored response for any repeated request carrying the
/// same `Idempotency-Key` header — the Rust analog of the Go port's
/// `IdempotencyMiddleware`. If the request body hashes differently from
/// the original, a 409 Conflict `ProblemDetail` (type
/// [`firefly_kernel::TYPE_IDEMPOTENCY`]) is returned per the IETF
/// idempotency-key draft. Replayed responses carry the original status,
/// headers, and body, plus an `Idempotent-Replay: true` marker. Only
/// 2xx responses are persisted. First-pass responses stream through
/// unbuffered while being captured, like the Go `captureWriter`.
#[derive(Clone)]
pub struct IdempotencyLayer {
    config: Arc<IdempotencyConfig>,
}

impl IdempotencyLayer {
    /// Returns a layer using `config`, normalizing a zero TTL to the
    /// canonical 24 hours.
    pub fn new(mut config: IdempotencyConfig) -> Self {
        if config.ttl == Duration::ZERO {
            config.ttl = Duration::from_secs(24 * 60 * 60);
        }
        Self {
            config: Arc::new(config),
        }
    }
}

impl Default for IdempotencyLayer {
    /// A layer over [`IdempotencyConfig::default`].
    fn default() -> Self {
        Self::new(IdempotencyConfig::default())
    }
}

impl<S> Layer<S> for IdempotencyLayer {
    type Service = IdempotencyService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        IdempotencyService {
            inner,
            config: Arc::clone(&self.config),
        }
    }
}

/// The tower service produced by [`IdempotencyLayer`].
#[derive(Clone)]
pub struct IdempotencyService<S> {
    inner: S,
    config: Arc<IdempotencyConfig>,
}

impl<S> Service<Request<Body>> for IdempotencyService<S>
where
    S: Service<Request<Body>, Response = Response, Error = Infallible> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = Infallible;
    type Future = BoxFuture<'static, Result<Response, Infallible>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let config = Arc::clone(&self.config);
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        Box::pin(async move {
            let key = req
                .headers()
                .get(&*IDEMPOTENCY_HEADER)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_owned();
            if key.is_empty() || !config.methods.contains(req.method()) {
                return inner.call(req).await;
            }

            // Buffer the request body so it can be hashed and restored.
            let (parts, body) = req.into_parts();
            let body_bytes = match to_bytes(body, usize::MAX).await {
                Ok(b) => b,
                Err(e) => {
                    return Ok(problem_response(&ProblemDetail::bad_request(format!(
                        "read body: {e}"
                    ))));
                }
            };
            let req_hash = hash_bytes(&body_bytes);

            // A store error falls through to the inner service, exactly
            // like the Go middleware (`err == nil && found`).
            if let Ok(Some(rec)) = config.store.get(&key).await {
                if rec.request_hash != req_hash {
                    return Ok(problem_response(
                        &FireflyError::idempotency_conflict(
                            "idempotency-key reused with different payload",
                        )
                        .to_problem(),
                    ));
                }
                return Ok(replay(&rec));
            }

            let req = Request::from_parts(parts, Body::from(body_bytes));
            let res = inner.call(req).await?;

            // Only successful (2xx) responses are persisted; anything
            // else streams through untouched, like the Go middleware.
            if !res.status().is_success() {
                return Ok(res);
            }

            // Capture status and headers (Go captures them at
            // `WriteHeader` time), buffer the full body, persist the
            // replay record, then return the complete response. This
            // mirrors the Go middleware's `captureWriter` + `Put`-before-
            // return ordering exactly: a retry arriving after the
            // response is guaranteed to observe the stored record.
            //
            // Buffering (rather than a streaming tee) is deliberate and
            // required for correctness: a transport that frames the body
            // by the handler's `Content-Length` header reads exactly that
            // many bytes and never polls the response body for its
            // end-of-stream signal, so an EOS-triggered persist would
            // silently never run. Idempotent (replayable) responses are
            // small, complete documents, so buffering costs nothing here.
            let (res_parts, res_body) = res.into_parts();
            let mut headers = BTreeMap::new();
            for (name, value) in &res_parts.headers {
                if let Ok(v) = value.to_str() {
                    headers
                        .entry(name.as_str().to_owned())
                        .or_insert_with(|| v.to_owned());
                }
            }
            let body_bytes = match to_bytes(res_body, usize::MAX).await {
                Ok(b) => b,
                // A body that errors mid-collection must not be replayed;
                // surface a problem rather than persisting a truncated record.
                Err(e) => {
                    return Ok(problem_response(&ProblemDetail::bad_request(format!(
                        "capture response body: {e}"
                    ))));
                }
            };
            let rec = IdempotencyRecord {
                status_code: res_parts.status.as_u16(),
                headers,
                body_hash: hash_bytes(&body_bytes),
                body: body_bytes.to_vec(),
                stored_at: Utc::now(),
                request_hash: req_hash,
            };
            // Store before returning (Go's `Put` before `ServeHTTP`
            // returns); a store error is swallowed like the Go middleware.
            let _ = config.store.put(&key, rec, config.ttl).await;
            Ok(Response::from_parts(res_parts, Body::from(body_bytes)))
        })
    }
}

/// Rebuilds the stored response and marks it with `Idempotent-Replay: true`.
fn replay(rec: &IdempotencyRecord) -> Response {
    let mut res = Response::new(Body::from(rec.body.clone()));
    *res.status_mut() = StatusCode::from_u16(rec.status_code).unwrap_or(StatusCode::OK);
    for (k, v) in &rec.headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) {
            res.headers_mut().insert(name, value);
        }
    }
    res.headers_mut()
        .insert(REPLAY_HEADER.clone(), HeaderValue::from_static("true"));
    res
}

/// Hex-encoded SHA-256, matching the Go port's `hashBytes`.
fn hash_bytes(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
