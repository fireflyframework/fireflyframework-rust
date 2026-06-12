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

//! The [`SseWriter`] / [`SseResponse`] pair, heartbeat loop, and the
//! `Last-Event-Id` resumption helper.

use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures::StreamExt;
use http::header::{HeaderName, HeaderValue, CACHE_CONTROL, CONNECTION, CONTENT_TYPE};
use http::HeaderMap;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::event::Event;

/// Canonical spelling of the resumption header, as written by browsers
/// and by the Go port (`r.Header.Get("Last-Event-Id")`). Lookups through
/// [`last_event_id`] are case-insensitive regardless.
pub const LAST_EVENT_ID_HEADER: &str = "Last-Event-Id";

/// Errors surfaced by [`SseWriter::send`].
///
/// Where Go's `Send` returns the underlying `net.Conn` write error once
/// the peer goes away, this port surfaces [`SseError::Disconnected`]
/// when the response stream has been dropped. Go's `ErrUnsupported`
/// (a `ResponseWriter` without `http.Flusher`) has no Rust counterpart:
/// axum bodies always stream, so flushing can never be unsupported.
#[derive(Debug, thiserror::Error)]
pub enum SseError {
    /// The client disconnected — the [`SseResponse`] body was dropped,
    /// so the event can never reach the wire.
    #[error("firefly/sse: client disconnected")]
    Disconnected,
}

/// Returns the value of the `Last-Event-Id` header, used by clients to
/// resume a stream from the last event they saw. `None` when the header
/// is absent (the Go helper returns `""`).
///
/// ```
/// use firefly_sse::last_event_id;
///
/// let headers = http::HeaderMap::new();
/// assert_eq!(last_event_id(&headers), None);
/// ```
pub fn last_event_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get(LAST_EVENT_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

/// Shared state between the writer handle and the heartbeat task.
struct Inner {
    /// `Some` while the stream is open; [`SseWriter::close`] takes the
    /// sender so the response body reaches EOF.
    tx: Mutex<Option<mpsc::UnboundedSender<Bytes>>>,
    closed: AtomicBool,
}

/// A single-connection SSE sink — the Rust counterpart of the Go port's
/// `sse.Writer`.
///
/// [`SseWriter::new`] returns the writer together with an
/// [`SseResponse`]: return the response from your axum handler and keep
/// the writer (typically inside a spawned task) to push events. Every
/// [`send`](SseWriter::send) enqueues a fully formatted frame on the
/// response stream; the optional heartbeat task shares the same channel
/// so its pings interleave safely — the analog of Go's shared mutex.
///
/// When the client disconnects, the response body is dropped and
/// subsequent sends return [`SseError::Disconnected`] — the analog of
/// Go's request-context cancellation. Dropping the writer (without
/// calling [`close`](SseWriter::close)) also stops the heartbeat and
/// ends the stream.
pub struct SseWriter {
    inner: Arc<Inner>,
    /// Signals the heartbeat task to exit promptly on close.
    done: watch::Sender<bool>,
    /// Heartbeat task handle; awaited by [`SseWriter::close`] so no ping
    /// can be written after close returns (Go: `wg.Wait()`).
    heartbeat: Mutex<Option<JoinHandle<()>>>,
}

impl SseWriter {
    /// Creates a writer and its paired streaming response.
    ///
    /// `ping_interval > 0` enables a heartbeat comment line
    /// (`: ping <unix-seconds>`) every interval — most proxies drop idle
    /// SSE streams after 30–60 s. Pass [`Duration::ZERO`] to disable
    /// heartbeats, exactly like the Go `NewWriter(rw, r, 0)`.
    ///
    /// Must be called within a tokio runtime when heartbeats are enabled
    /// (the heartbeat runs as a spawned task).
    pub fn new(ping_interval: Duration) -> (SseWriter, SseResponse) {
        let (tx, rx) = mpsc::unbounded_channel();
        let inner = Arc::new(Inner {
            tx: Mutex::new(Some(tx)),
            closed: AtomicBool::new(false),
        });
        let (done, done_rx) = watch::channel(false);
        let heartbeat = if ping_interval > Duration::ZERO {
            Some(spawn_heartbeat(Arc::clone(&inner), ping_interval, done_rx))
        } else {
            None
        };
        (
            SseWriter {
                inner,
                done,
                heartbeat: Mutex::new(heartbeat),
            },
            SseResponse { rx },
        )
    }

    /// Writes the event to the stream. Safe to call after
    /// [`close`](SseWriter::close) — returns `Ok(())` silently, exactly
    /// like the Go `Send` on a closed writer. Returns
    /// [`SseError::Disconnected`] once the client has gone away.
    pub fn send(&self, ev: Event) -> Result<(), SseError> {
        if self.inner.closed.load(Ordering::SeqCst) {
            return Ok(());
        }
        let guard = self.inner.tx.lock().expect("sse writer mutex poisoned");
        match guard.as_ref() {
            Some(tx) => tx
                .send(Bytes::from(ev.to_wire()))
                .map_err(|_| SseError::Disconnected),
            None => Ok(()),
        }
    }

    /// `true` once [`close`](SseWriter::close) has been called.
    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::SeqCst)
    }

    /// Stops the heartbeat (if any), ends the response stream, and waits
    /// for the heartbeat task to exit before returning — eliminating the
    /// post-close write race, exactly like the Go `Close`. Safe to call
    /// multiple times.
    pub async fn close(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        // Dropping the sender lets the response body reach EOF.
        drop(
            self.inner
                .tx
                .lock()
                .expect("sse writer mutex poisoned")
                .take(),
        );
        let _ = self.done.send(true);
        let handle = self
            .heartbeat
            .lock()
            .expect("sse heartbeat mutex poisoned")
            .take();
        if let Some(handle) = handle {
            let _ = handle.await;
        }
    }
}

/// Emits `: ping <unix-seconds>` comment frames every `every` until the
/// writer closes or the client disconnects — the port of Go's
/// `pingLoop` goroutine.
fn spawn_heartbeat(
    inner: Arc<Inner>,
    every: Duration,
    mut done: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(every);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        // tokio's first tick completes immediately; consume it so the
        // first ping lands after one full interval, like Go's Ticker.
        ticker.tick().await;
        loop {
            tokio::select! {
                // Resolves on close() — and on writer drop, when the
                // watch sender goes away.
                _ = done.changed() => return,
                _ = ticker.tick() => {
                    if inner.closed.load(Ordering::SeqCst) {
                        return;
                    }
                    let unix = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let frame = Bytes::from(format!(": ping {unix}\n\n"));
                    let sent = {
                        let guard = inner.tx.lock().expect("sse writer mutex poisoned");
                        match guard.as_ref() {
                            Some(tx) => tx.send(frame).is_ok(),
                            None => false,
                        }
                    };
                    if !sent {
                        return;
                    }
                }
            }
        }
    })
}

/// The streaming half of an [`SseWriter`] pair. Return it from an axum
/// handler; its [`IntoResponse`] impl sets the exact headers the Go
/// `NewWriter` writes:
///
/// | Header              | Value               |
/// |---------------------|---------------------|
/// | `Content-Type`      | `text/event-stream` |
/// | `Cache-Control`     | `no-cache`          |
/// | `Connection`        | `keep-alive`        |
/// | `X-Accel-Buffering` | `no` (nginx)        |
///
/// The body streams every frame the writer sends and ends when the
/// writer is closed or dropped.
pub struct SseResponse {
    pub(crate) rx: mpsc::UnboundedReceiver<Bytes>,
}

impl IntoResponse for SseResponse {
    fn into_response(self) -> Response {
        let stream = UnboundedReceiverStream::new(self.rx).map(Ok::<Bytes, Infallible>);
        let mut response = Response::new(Body::from_stream(stream));
        let headers = response.headers_mut();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
        headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        headers.insert(CONNECTION, HeaderValue::from_static("keep-alive"));
        headers.insert(
            HeaderName::from_static("x-accel-buffering"),
            HeaderValue::from_static("no"),
        );
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use axum::Router;
    use http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    /// Port of the Go test handler: two events, then close.
    async fn send_two_events() -> SseResponse {
        let (writer, response) = SseWriter::new(Duration::ZERO);
        tokio::spawn(async move {
            writer
                .send(Event {
                    id: "1".into(),
                    event: "ping".into(),
                    data: "hello".into(),
                    ..Event::default()
                })
                .unwrap();
            writer
                .send(Event {
                    data: "line1\nline2".into(),
                    ..Event::default()
                })
                .unwrap();
            writer.close().await;
        });
        response
    }

    /// Port of Go `TestSendEvent`.
    #[tokio::test]
    async fn send_event_emits_canonical_frames() {
        let app = Router::new().route("/", get(send_two_events));
        let response = app
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CONTENT_TYPE], "text/event-stream");

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let out = std::str::from_utf8(&body).unwrap();
        for want in [
            "id: 1",
            "event: ping",
            "data: hello",
            "data: line1",
            "data: line2",
        ] {
            assert!(out.contains(want), "missing {want:?} in:\n{out}");
        }
        // Stronger than the Go containment checks: the exact wire bytes.
        assert_eq!(
            out,
            "id: 1\nevent: ping\ndata: hello\n\ndata: line1\ndata: line2\n\n"
        );
    }

    async fn heartbeat_handler() -> SseResponse {
        let (writer, response) = SseWriter::new(Duration::from_millis(10));
        tokio::spawn(async move {
            // Hold the connection for ~50 ms so a couple of pings emit.
            tokio::time::sleep(Duration::from_millis(50)).await;
            writer.close().await;
        });
        response
    }

    /// Port of Go `TestPingHeartbeat`.
    #[tokio::test]
    async fn heartbeat_emits_ping_comments() {
        let app = Router::new().route("/", get(heartbeat_handler));
        let response = app
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let out = std::str::from_utf8(&body).unwrap();
        assert!(out.contains(": ping "), "expected heartbeat in:\n{out}");
    }

    /// The exact heartbeat frame shape: `: ping <unix-seconds>\n\n`.
    #[tokio::test]
    async fn heartbeat_frame_is_a_comment_with_unix_timestamp() {
        let (writer, mut response) = SseWriter::new(Duration::from_millis(5));
        let frame = response.rx.recv().await.expect("ping frame");
        let text = std::str::from_utf8(&frame).unwrap();
        assert!(text.starts_with(": ping "), "frame: {text:?}");
        assert!(text.ends_with("\n\n"), "frame: {text:?}");
        text.trim_start_matches(": ping ")
            .trim_end()
            .parse::<u64>()
            .expect("unix timestamp");
        writer.close().await;
    }

    /// Port of Go `TestLastEventID`.
    #[test]
    fn last_event_id_returns_header_value() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_bytes(b"Last-Event-Id").unwrap(),
            HeaderValue::from_static("evt-99"),
        );
        assert_eq!(last_event_id(&headers), Some("evt-99".to_string()));
    }

    #[test]
    fn last_event_id_missing_returns_none() {
        assert_eq!(last_event_id(&HeaderMap::new()), None);
    }

    #[tokio::test]
    async fn response_sets_all_sse_headers() {
        let (writer, response) = SseWriter::new(Duration::ZERO);
        let response = response.into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let h = response.headers();
        assert_eq!(h["content-type"], "text/event-stream");
        assert_eq!(h["cache-control"], "no-cache");
        assert_eq!(h["connection"], "keep-alive");
        assert_eq!(h["x-accel-buffering"], "no");
        writer.close().await;
    }

    #[tokio::test]
    async fn send_after_close_is_silently_ignored() {
        let (writer, response) = SseWriter::new(Duration::ZERO);
        writer.close().await;
        assert!(writer.is_closed());
        let result = writer.send(Event {
            data: "late".into(),
            ..Event::default()
        });
        assert!(result.is_ok());
        // Nothing reached the wire; the stream is already at EOF.
        let body = response.into_response().into_body();
        let bytes = body.collect().await.unwrap().to_bytes();
        assert!(bytes.is_empty(), "unexpected bytes: {bytes:?}");
    }

    #[tokio::test]
    async fn send_after_client_disconnect_returns_disconnected() {
        let (writer, response) = SseWriter::new(Duration::ZERO);
        drop(response);
        let err = writer
            .send(Event {
                data: "x".into(),
                ..Event::default()
            })
            .unwrap_err();
        assert!(matches!(err, SseError::Disconnected));
        writer.close().await;
    }

    #[tokio::test]
    async fn close_is_idempotent_and_waits_for_heartbeat() {
        let (writer, _response) = SseWriter::new(Duration::from_millis(5));
        writer.close().await;
        writer.close().await; // second close: no panic, no hang
        assert!(writer.is_closed());
    }

    #[tokio::test]
    async fn close_stops_heartbeat_and_ends_stream() {
        let (writer, mut response) = SseWriter::new(Duration::from_millis(5));
        writer.close().await;
        // close() awaited the heartbeat task, so no ping can follow; the
        // sender is dropped, so the stream ends immediately.
        assert_eq!(response.rx.recv().await, None);
    }

    #[test]
    fn error_message_matches_port_convention() {
        assert_eq!(
            SseError::Disconnected.to_string(),
            "firefly/sse: client disconnected",
        );
    }
}
