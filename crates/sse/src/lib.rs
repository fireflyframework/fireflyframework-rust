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

//! # firefly-sse
//!
//! A **Server-Sent Events writer** — the lightweight half-duplex
//! equivalent of WebSockets, ideal for live dashboards, log tailing,
//! and CQRS read-side cache invalidation pushes. The Rust counterpart
//! of Spring's `ServerSentEvent` and the Go port's `sse` module.
//!
//! [`SseWriter::new`] returns a writer plus a streaming [`SseResponse`]
//! to return from an axum handler. Each [`SseWriter::send`] enqueues a
//! fully formatted frame; an optional heartbeat task emits a
//! `: ping <unix-seconds>` comment at a configurable interval to keep
//! proxies from dropping the connection.
//!
//! ## Wire format
//!
//! The writer emits the canonical SSE syntax, byte-for-byte identical
//! to the Java, .NET, Go, and Python ports:
//!
//! ```text
//! retry: 5000
//! id: evt-42
//! event: order
//! data: {"id":"o1","customer":"alice"}
//!
//! ```
//!
//! Each [`Event`] ends with a blank line; `data` containing newlines is
//! split into multiple `data:` lines per the spec.
//!
//! ## Resumption
//!
//! Clients reconnect with `Last-Event-Id: <id>` to resume from the last
//! seen event — use [`last_event_id`] on the request headers to look up
//! a starting position before pulling events to send.
//!
//! ## Adaptation from Go
//!
//! Go wraps an `http.ResponseWriter` and flushes after every write;
//! axum inverts the flow: the writer feeds a channel-backed streaming
//! body, so every frame hits the wire as soon as it is sent — no
//! `http.Flusher` (and therefore no `ErrUnsupported`) needed. Client
//! disconnects surface as [`SseError::Disconnected`] from `send`, the
//! analog of Go's request-context cancellation.
//!
//! ## Quick start
//!
//! ```
//! use std::time::Duration;
//! use axum::{routing::get, Router};
//! use firefly_sse::{Event, SseResponse, SseWriter};
//!
//! async fn live_orders() -> SseResponse {
//!     // 30 s heartbeat keeps proxies from dropping the idle stream.
//!     let (writer, response) = SseWriter::new(Duration::from_secs(30));
//!     tokio::spawn(async move {
//!         let _ = writer.send(Event {
//!             id: "evt-42".into(),
//!             event: "order".into(),
//!             data: r#"{"id":"o1","customer":"alice"}"#.into(),
//!             ..Event::default()
//!         });
//!         writer.close().await;
//!     });
//!     response
//! }
//!
//! let app: Router = Router::new().route("/orders/live", get(live_orders));
//! # let _ = app;
//! ```

mod event;
mod writer;

pub use event::Event;
pub use writer::{last_event_id, SseError, SseResponse, SseWriter, LAST_EVENT_ID_HEADER};

/// Released framework version. Calendar-versioned (`YY.M.PATCH`), the
/// Rust port's counterpart of the Go `kernel.Version` constant.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn public_types_are_send_and_sync() {
        assert_send_sync::<Event>();
        assert_send_sync::<SseWriter>();
        assert_send_sync::<SseResponse>();
        assert_send_sync::<SseError>();
    }

    #[test]
    fn version_matches_crate_version() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }
}
