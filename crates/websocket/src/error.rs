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

//! Error type for WebSocket session operations.

use thiserror::Error;

/// Errors raised by [`WsSession`](crate::WsSession) send/receive operations.
///
/// The pyfly port surfaces a `WebSocketDisconnect` exception on the
/// receiving side and lets unexpected failures propagate to the registrar's
/// `try/except`. The Rust port collapses those into a single typed enum so a
/// handler can `match` on the cause and decide whether to clean up, retry, or
/// bail. [`WsError::Disconnected`] is the structural analog of Starlette's
/// `WebSocketDisconnect` — it is the normal way a `recv_*` loop terminates and
/// should generally be treated as an expected end-of-stream, not a failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum WsError {
    /// The peer closed the connection or the stream ended.
    ///
    /// Returned by every `recv_*` method when the socket yields a `Close`
    /// frame or `None`. Mirrors pyfly's `WebSocketDisconnect`.
    #[error("websocket disconnected")]
    Disconnected,

    /// A frame of an unexpected kind was received where another was expected
    /// (for example a binary frame returned to `recv_text`, or vice versa).
    #[error("websocket protocol error: {0}")]
    Protocol(String),

    /// The underlying transport reported an I/O / WebSocket error while
    /// sending or receiving.
    #[error("websocket transport error: {0}")]
    Transport(String),

    /// A JSON payload could not be serialised (on `send_json`) or
    /// deserialised (on `recv_json`).
    #[error("websocket JSON error: {0}")]
    Serde(#[from] serde_json::Error),
}
