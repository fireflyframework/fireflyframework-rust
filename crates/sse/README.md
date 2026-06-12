# `firefly-sse`

> **Tier:** Platform · **Status:** Full · **Java original:** Spring `ServerSentEvent` · **Go module:** `sse`

## Overview

`firefly-sse` provides a **Server-Sent Events writer** — the lightweight
half-duplex equivalent of WebSockets, ideal for live dashboards, log
tailing, and CQRS read-side cache invalidation pushes.
`SseWriter::new(ping_interval)` returns a writer plus a streaming
`SseResponse` to return from an axum handler. Each `send(Event)`
enqueues a fully formatted frame on the response stream — frames hit
the wire as soon as they are sent — and an optional heartbeat task
emits a ping comment at a configurable interval to keep proxies from
dropping the connection.

```rust
use std::time::Duration;
use axum::{routing::get, Router};
use firefly_sse::{Event, SseResponse, SseWriter};

async fn live_orders() -> SseResponse {
    // 30 s heartbeat keeps proxies from dropping the idle stream.
    let (writer, response) = SseWriter::new(Duration::from_secs(30));
    tokio::spawn(async move {
        let _ = writer.send(Event {
            id: "evt-42".into(),
            event: "order".into(),
            data: r#"{"id":"o1","customer":"alice"}"#.into(),
            ..Event::default()
        });
        writer.close().await;
    });
    response
}

let app: Router = Router::new().route("/orders/live", get(live_orders));
```

## Wire format

The writer emits the canonical SSE syntax, byte-for-byte identical to
the Java, .NET, Go, and Python ports:

```
retry: 5000
id: evt-42
event: order
data: {"id":"o1","customer":"alice"}

```

Each `Event` ends with a blank line; `data` containing newlines is
split into multiple `data:` lines per the spec. The heartbeat is a
comment frame: `: ping <unix-seconds>`. The response carries the same
headers Go's `NewWriter` writes: `Content-Type: text/event-stream`,
`Cache-Control: no-cache`, `Connection: keep-alive`, and
`X-Accel-Buffering: no` (disables nginx buffering).

## Resumption

Clients reconnect with `Last-Event-Id: <id>` to resume from the last
seen event. Use the helper:

```rust,ignore
let since = firefly_sse::last_event_id(request.headers()); // None when not present
```

…to look up a starting position before pulling events to send.

## Public surface

```rust,ignore
pub struct Event {
    pub id: String,    // optional; clients reconnect with Last-Event-Id when set
    pub event: String, // optional event name (defaults to "message")
    pub data: String,  // payload — may contain newlines (the writer splits them)
    pub retry: u64,    // optional millisecond reconnect hint; 0 omits it
}
impl Event {
    pub fn to_wire(&self) -> String; // exact SSE frame bytes
}

pub struct SseWriter { /* … */ }
impl SseWriter {
    pub fn new(ping_interval: Duration) -> (SseWriter, SseResponse);
    pub fn send(&self, ev: Event) -> Result<(), SseError>;
    pub fn is_closed(&self) -> bool;
    pub async fn close(&self); // idempotent; waits for the heartbeat task
}

pub struct SseResponse { /* … */ } // impl IntoResponse — return from a handler

pub enum SseError { Disconnected }

pub fn last_event_id(headers: &http::HeaderMap) -> Option<String>;
pub const LAST_EVENT_ID_HEADER: &str = "Last-Event-Id";
```

Pass `ping_interval = Duration::ZERO` to disable heartbeats, exactly
like the Go `NewWriter(rw, r, 0)`.

## Adaptation from Go

Go wraps an `http.ResponseWriter` and flushes after every write; axum
inverts the flow — the writer feeds a channel-backed streaming body, so
flushing is implicit and Go's `ErrUnsupported` (a `ResponseWriter`
without `http.Flusher`) has no Rust counterpart. Client disconnects
surface as `SseError::Disconnected` from `send`, the analog of Go's
request-context cancellation; `send` after `close` returns `Ok(())`
silently, matching Go's nil return on a closed writer.

## Concurrency

`SseWriter` is `Send + Sync`; sends serialise on an internal mutex,
and the heartbeat task (when enabled) shares the same channel so its
pings interleave safely. `close` signals the heartbeat task and waits
for it to exit before returning — eliminating the post-close write
race — and is safe to call multiple times. Dropping the writer without
closing also stops the heartbeat and ends the stream.

## Testing

```bash
cargo test -p firefly-sse
```

Covers single-event emission with exact wire bytes, multi-line `data:`
splitting, heartbeat emission and frame shape, `Last-Event-Id` lookup,
the SSE response headers, the silent post-close `send`, the
disconnected-client error, and the race-clean idempotent `close` path —
HTTP cases driven in-process through `tower::ServiceExt::oneshot`, no
sockets.
