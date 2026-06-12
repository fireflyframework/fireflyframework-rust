//! Reactive (WebFlux / Reactor-style) HTTP responders built on
//! [`firefly_reactive`] — the Rust analog of returning `Mono<T>` /
//! `Flux<T>` from a Spring WebFlux `@RestController`.
//!
//! This module is **strictly additive**: it sits alongside the existing
//! middleware surface and reuses the crate's RFC 7807 [`problem_response`]
//! renderer plus `firefly-sse`'s wire format, so every reactive response
//! is byte-compatible with the rest of the framework.
//!
//! | Spring WebFlux                              | firefly-web                                   |
//! |---------------------------------------------|-----------------------------------------------|
//! | `Mono<T>` handler return                    | [`MonoJson`]`(Mono<T>)` via [`IntoResponse`]  |
//! | `Mono<T>` empty → `404`                     | `Ok(None)` → `application/problem+json` 404   |
//! | `Mono<T>` error → problem                   | `Err(FireflyError)` → that error's problem    |
//! | `Flux<T>` + `application/x-ndjson`          | [`NdJson`]`(Flux<T>)`                          |
//! | `Flux<ServerSentEvent<T>>`                  | [`Sse`]`(Flux<T>)` / [`SseEvents`]            |
//!
//! ## Backpressure
//!
//! [`NdJson`] and [`Sse`] bridge the `Flux`'s underlying `Stream`
//! straight into an axum streaming [`Body`] — one frame per element,
//! flushed incrementally. The whole stream is **never** buffered, so a
//! slow client throttles the producer through axum's body backpressure,
//! exactly like WebFlux's `Flux` write path. An [`Err`] item mid-stream
//! terminates the response body cleanly (the connection closes; no
//! trailing frame), mirroring Reactor's terminal `onError`.
//!
//! ## Resolving a `Mono`
//!
//! `IntoResponse::into_response` is synchronous, but a [`Mono`] is async.
//! Returning a [`Mono`] from a handler resolves it on the current Tokio
//! worker via [`tokio::task::block_in_place`] (the framework's default
//! runtime is multi-threaded), so the HTTP status faithfully reflects the
//! terminal signal — `200`, `404`, or the error's problem status. For an
//! explicitly streamed body, prefer [`NdJson`] / [`Sse`].

use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::response::{IntoResponse, Response};
use bytes::{BufMut, Bytes, BytesMut};
use firefly_kernel::{FireflyError, ProblemDetail};
use firefly_reactive::{Flux, Mono};
use firefly_sse::Event as SseEvent;
use futures::Stream;
use http::{header, HeaderValue, StatusCode};
use serde::Serialize;

use crate::problem::problem_response;

/// The `Content-Type` of the NDJSON streaming responder, matching the
/// Spring WebFlux `MediaType.APPLICATION_NDJSON_VALUE` constant.
pub const NDJSON_CONTENT_TYPE: &str = "application/x-ndjson";

/// The `Content-Type` of the SSE streaming responder — the same value
/// `firefly-sse` writes.
pub const SSE_CONTENT_TYPE: &str = "text/event-stream";

// --------------------------------------------------------------------
// Mono<T> → response
// --------------------------------------------------------------------

/// Renders a resolved [`Mono`] terminal signal into a [`Response`],
/// reused by every `IntoResponse for Mono<_>` impl.
///
/// - `Ok(Some(v))` → `200 OK`, JSON body (`v` serialized).
/// - `Ok(None)`    → `404`, `application/problem+json` via [`problem_response`].
/// - `Err(e)`      → the error's own RFC 7807 problem response.
fn mono_outcome_response<T: Serialize>(outcome: Result<Option<T>, FireflyError>) -> Response {
    match outcome {
        Ok(Some(value)) => match serde_json::to_vec(&value) {
            Ok(bytes) => {
                let mut res = Response::new(Body::from(bytes));
                *res.status_mut() = StatusCode::OK;
                res.headers_mut().insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );
                res
            }
            Err(err) => problem_response(&ProblemDetail::internal(format!(
                "serialization failed: {err}"
            ))),
        },
        Ok(None) => problem_response(&ProblemDetail::not_found("resource not found")),
        Err(err) => problem_response(&err.to_problem()),
    }
}

/// Drives a [`Mono`] to its terminal signal without parking the executor
/// thread when called from inside a multi-threaded Tokio runtime.
///
/// On a multi-thread runtime (the framework default) this hands the
/// future to [`tokio::task::block_in_place`], so other tasks keep making
/// progress. Outside any runtime it falls back to a transient
/// current-thread runtime — handy for unit tests and non-async callers.
fn resolve_mono<T: Send + 'static>(mono: Mono<T>) -> Result<Option<T>, FireflyError> {
    use tokio::runtime::{Handle, RuntimeFlavor};
    match Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(|| handle.block_on(mono.into_future()))
        }
        Ok(handle) => {
            // Current-thread runtime: spawning a fresh thread with its own
            // runtime avoids the `block_on`-from-within-runtime panic.
            let _ = handle;
            std::thread::scope(|scope| {
                scope
                    .spawn(|| {
                        tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .expect("transient runtime")
                            .block_on(mono.into_future())
                    })
                    .join()
                    .expect("mono resolver thread panicked")
            })
        }
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("transient runtime")
            .block_on(mono.into_future()),
    }
}

/// A newtype letting a handler return a [`Mono<T>`] while keeping
/// `IntoResponse` coherent — the Rust analog of a WebFlux handler whose
/// return type is `Mono<T>`.
///
/// Both [`firefly_reactive::Mono`] and [`axum::Json`] live in other
/// crates, so the orphan rule forbids `impl IntoResponse for Mono<T>`
/// (or `Mono<Json<T>>`) directly. This newtype is the bridge:
///
/// - `Ok(Some(v))` → `200 OK` with `v` serialized as `application/json`
///   (the `Json`-equivalent path — the value is JSON-encoded for you);
/// - `Ok(None)` → `404` `application/problem+json`;
/// - `Err(e)` → the error's own RFC 7807 problem response.
///
/// ```
/// use axum::response::IntoResponse;
/// use firefly_reactive::Mono;
/// use firefly_web::reactive::MonoJson;
///
/// // The WebFlux `Mono<T>` return, spelled as `MonoJson(mono)`.
/// async fn handler() -> impl IntoResponse {
///     MonoJson(Mono::just(serde_json::json!({"ok": true})))
/// }
/// # let _ = handler;
/// ```
pub struct MonoJson<T>(pub Mono<T>);

impl<T> From<Mono<T>> for MonoJson<T> {
    fn from(mono: Mono<T>) -> Self {
        Self(mono)
    }
}

impl<T> IntoResponse for MonoJson<T>
where
    T: Serialize + Send + 'static,
{
    fn into_response(self) -> Response {
        mono_outcome_response(resolve_mono(self.0))
    }
}

// --------------------------------------------------------------------
// Flux<T> → application/x-ndjson
// --------------------------------------------------------------------

/// A streaming `application/x-ndjson` responder over a [`Flux<T>`] — the
/// Rust analog of a WebFlux handler returning `Flux<T>` with
/// `produces = APPLICATION_NDJSON_VALUE`.
///
/// Each element is serialized to a compact JSON document followed by a
/// single `'\n'` and flushed **incrementally**: the [`Flux`]'s underlying
/// `Stream` is bridged straight into an axum streaming [`Body`], so the
/// whole stream is never buffered and a slow consumer applies real
/// backpressure to the producer. An [`Err`] item terminates the body
/// cleanly — the bytes emitted before it stay on the wire, then the
/// stream ends (Reactor's terminal `onError`).
///
/// ```
/// use axum::response::IntoResponse;
/// use firefly_reactive::Flux;
/// use firefly_web::reactive::NdJson;
///
/// async fn handler() -> impl IntoResponse {
///     NdJson(Flux::just(vec![1, 2, 3]))
/// }
/// # let _ = handler;
/// ```
pub struct NdJson<T>(pub Flux<T>);

impl<T> IntoResponse for NdJson<T>
where
    T: Serialize + Send + 'static,
{
    fn into_response(self) -> Response {
        let body = Body::from_stream(NdJsonStream {
            inner: self.0.into_stream(),
        });
        let mut res = Response::new(body);
        res.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(NDJSON_CONTENT_TYPE),
        );
        res
    }
}

/// Adapts the `Flux`'s `Result<T, FireflyError>` item stream into the
/// `Result<Bytes, Infallible>` chunks axum's `Body::from_stream` wants:
/// each `Ok(T)` becomes one `<json>\n` chunk; the first `Err` ends the
/// stream (so the body terminates cleanly mid-flight).
struct NdJsonStream<T> {
    inner: Pin<Box<dyn Stream<Item = Result<T, FireflyError>> + Send>>,
}

impl<T> Stream for NdJsonStream<T>
where
    T: Serialize,
{
    type Item = Result<Bytes, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(item))) => {
                let mut buf = BytesMut::new();
                match serde_json::to_writer((&mut buf).writer(), &item) {
                    Ok(()) => {
                        buf.put_u8(b'\n');
                        Poll::Ready(Some(Ok(buf.freeze())))
                    }
                    // A doc that fails to serialize can't be framed; end
                    // the stream cleanly rather than emit a partial line.
                    Err(_) => Poll::Ready(None),
                }
            }
            // Terminal error: stop the body so the half-written response
            // closes without a trailing frame (Reactor onError).
            Poll::Ready(Some(Err(_))) => Poll::Ready(None),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

// --------------------------------------------------------------------
// Flux<T> → text/event-stream (SSE)
// --------------------------------------------------------------------

/// A streaming `text/event-stream` responder over a [`Flux<T>`], reusing
/// `firefly-sse`'s [`Event`](firefly_sse::Event) wire format — the Rust
/// analog of a WebFlux handler returning `Flux<ServerSentEvent<T>>`.
///
/// Each element is serialized to JSON and emitted as a single `data:`
/// frame (`data: <json>\n\n`) via [`firefly_sse::Event::to_wire`], so the
/// bytes are byte-identical to the `firefly-sse` writer and every other
/// runtime's SSE output. Like [`NdJson`], frames flush incrementally with
/// real backpressure and an [`Err`] item ends the stream cleanly.
///
/// To attach `id` / `event` / `retry` fields, pre-map the `Flux` to
/// [`firefly_sse::Event`] values and use [`SseEvents`] instead.
///
/// ```
/// use axum::response::IntoResponse;
/// use firefly_reactive::Flux;
/// use firefly_web::reactive::Sse;
///
/// async fn handler() -> impl IntoResponse {
///     Sse(Flux::just(vec![1, 2, 3]))
/// }
/// # let _ = handler;
/// ```
pub struct Sse<T>(pub Flux<T>);

impl<T> IntoResponse for Sse<T>
where
    T: Serialize + Send + 'static,
{
    fn into_response(self) -> Response {
        let stream = SseDataStream {
            inner: self.0.into_stream(),
        };
        sse_response(stream)
    }
}

/// A streaming SSE responder over a [`Flux`] of fully-formed
/// [`firefly_sse::Event`] values — use when you need `id` / `event` /
/// `retry` fields. Each event is encoded with
/// [`firefly_sse::Event::to_wire`], identical to the `firefly-sse`
/// writer.
///
/// ```
/// use axum::response::IntoResponse;
/// use firefly_reactive::Flux;
/// use firefly_sse::Event;
/// use firefly_web::reactive::SseEvents;
///
/// async fn handler() -> impl IntoResponse {
///     SseEvents(Flux::just(vec![Event {
///         event: "tick".into(),
///         data: "1".into(),
///         ..Event::default()
///     }]))
/// }
/// # let _ = handler;
/// ```
pub struct SseEvents(pub Flux<SseEvent>);

impl IntoResponse for SseEvents {
    fn into_response(self) -> Response {
        let stream = SseEventStream {
            inner: self.0.into_stream(),
        };
        sse_response(stream)
    }
}

/// Builds the SSE response shell (status `200`, `text/event-stream`,
/// `no-cache`, `keep-alive`, nginx `X-Accel-Buffering: no`) around a
/// frame stream — the same headers `firefly-sse` writes.
fn sse_response<S>(stream: S) -> Response
where
    S: Stream<Item = Result<Bytes, Infallible>> + Send + 'static,
{
    let mut res = Response::new(Body::from_stream(stream));
    let headers = res.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(SSE_CONTENT_TYPE),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
    headers.insert(
        http::HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    res
}

/// Serializes each `Flux` element to JSON and frames it as a bare
/// `data:` SSE event.
struct SseDataStream<T> {
    inner: Pin<Box<dyn Stream<Item = Result<T, FireflyError>> + Send>>,
}

impl<T> Stream for SseDataStream<T>
where
    T: Serialize,
{
    type Item = Result<Bytes, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(item))) => match serde_json::to_string(&item) {
                Ok(data) => {
                    let frame = SseEvent {
                        data,
                        ..SseEvent::default()
                    };
                    Poll::Ready(Some(Ok(Bytes::from(frame.to_wire()))))
                }
                Err(_) => Poll::Ready(None),
            },
            Poll::Ready(Some(Err(_))) => Poll::Ready(None),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Encodes each pre-built [`firefly_sse::Event`] with its canonical wire
/// format.
struct SseEventStream {
    inner: Pin<Box<dyn Stream<Item = Result<SseEvent, FireflyError>> + Send>>,
}

impl Stream for SseEventStream {
    type Item = Result<Bytes, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(event))) => Poll::Ready(Some(Ok(Bytes::from(event.to_wire())))),
            Poll::Ready(Some(Err(_))) => Poll::Ready(None),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}
