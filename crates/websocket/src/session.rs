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

//! [`WsSession`] — a typed handle over an accepted axum WebSocket.

use std::collections::HashMap;

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use serde::{de::DeserializeOwned, Serialize};
use uuid::Uuid;

use crate::error::WsError;

/// A clean, typed handle over a single accepted WebSocket connection.
///
/// `WsSession` is the Rust counterpart of pyfly's `WebSocketSession`: it wraps
/// the raw socket and exposes `send_text` / `send_json` / `send_bytes` and
/// `recv_text` / `recv_json` / `recv_bytes`, plus `close`. Each session carries
/// a stable [`id`](WsSession::id) (a UUID v4) and a free-form
/// [`metadata`](WsSession::metadata) map a handler can use to stash
/// per-connection state.
///
/// # Adaptation from pyfly
///
/// pyfly's session exposes an `accepted` flag and an `accept(subprotocol)`
/// method because Starlette hands the controller a *not-yet-accepted* socket
/// and the handler owns the handshake. axum inverts this: the upgrade callback
/// only runs **after** the handshake completes, so by the time a `WsSession`
/// exists the connection is already accepted. The `accepted` flag and `accept`
/// method are therefore unnecessary and omitted — the "only if accepted" gate
/// of the disconnect contract is satisfied structurally (see
/// [`serve_ws`](crate::serve_ws)). Subprotocol selection and path/query/header
/// access move to axum extractors (`WebSocketUpgrade::protocols`, `Path`,
/// `Query`, `HeaderMap`) at the route layer rather than to session properties.
///
/// The `recv_*` methods transparently skip `Ping`/`Pong` control frames and
/// return [`WsError::Disconnected`] on a `Close` frame or end-of-stream,
/// mirroring how pyfly raises `WebSocketDisconnect`.
pub struct WsSession {
    socket: WebSocket,
    id: String,
    metadata: HashMap<String, String>,
    closed: bool,
}

impl WsSession {
    /// Wrap a freshly-upgraded axum [`WebSocket`] in a session, assigning a
    /// fresh UUID v4 identifier.
    ///
    /// Called by [`serve_ws`](crate::serve_ws); construct directly only when
    /// driving the socket yourself.
    pub fn new(socket: WebSocket) -> Self {
        Self {
            socket,
            id: Uuid::new_v4().to_string(),
            metadata: HashMap::new(),
            closed: false,
        }
    }

    /// The stable per-connection identifier (UUID v4 string).
    ///
    /// Use it as the key when registering with a
    /// [`BroadcastHub`](crate::BroadcastHub).
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Immutable view of the per-connection metadata map.
    pub fn metadata(&self) -> &HashMap<String, String> {
        &self.metadata
    }

    /// Mutable view of the per-connection metadata map — stash arbitrary
    /// string state (auth subject, room name, …) scoped to this connection.
    pub fn metadata_mut(&mut self) -> &mut HashMap<String, String> {
        &mut self.metadata
    }

    /// Whether [`close`](WsSession::close) has already been sent.
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Send a UTF-8 text frame to the client.
    pub async fn send_text(&mut self, data: impl Into<String>) -> Result<(), WsError> {
        self.socket
            .send(Message::Text(data.into()))
            .await
            .map_err(|e| WsError::Transport(e.to_string()))
    }

    /// Serialise `value` to JSON and send it as a text frame.
    ///
    /// The wire format is a single text frame containing the compact JSON
    /// encoding — identical to pyfly's `send_json(..., mode="text")` default.
    pub async fn send_json<T: Serialize>(&mut self, value: &T) -> Result<(), WsError> {
        let body = serde_json::to_string(value)?;
        self.send_text(body).await
    }

    /// Send a binary frame to the client.
    pub async fn send_bytes(&mut self, data: impl Into<Vec<u8>>) -> Result<(), WsError> {
        self.socket
            .send(Message::Binary(data.into()))
            .await
            .map_err(|e| WsError::Transport(e.to_string()))
    }

    /// Receive the next text frame from the client.
    ///
    /// `Ping`/`Pong` control frames are skipped transparently. Returns
    /// [`WsError::Disconnected`] when the peer closes or the stream ends, and
    /// [`WsError::Protocol`] when a binary frame arrives where text was
    /// expected.
    pub async fn recv_text(&mut self) -> Result<String, WsError> {
        loop {
            match self.next_message().await? {
                Message::Text(text) => return Ok(text),
                Message::Binary(_) => {
                    return Err(WsError::Protocol("expected text frame, got binary".into()))
                }
                // Control frames are skipped; Close/None handled by next_message.
                _ => continue,
            }
        }
    }

    /// Receive the next text frame and deserialise it from JSON.
    pub async fn recv_json<T: DeserializeOwned>(&mut self) -> Result<T, WsError> {
        let text = self.recv_text().await?;
        Ok(serde_json::from_str(&text)?)
    }

    /// Receive the next binary frame from the client.
    ///
    /// `Ping`/`Pong` control frames are skipped transparently. Returns
    /// [`WsError::Disconnected`] when the peer closes or the stream ends, and
    /// [`WsError::Protocol`] when a text frame arrives where binary was
    /// expected.
    pub async fn recv_bytes(&mut self) -> Result<Vec<u8>, WsError> {
        loop {
            match self.next_message().await? {
                Message::Binary(data) => return Ok(data),
                Message::Text(_) => {
                    return Err(WsError::Protocol("expected binary frame, got text".into()))
                }
                _ => continue,
            }
        }
    }

    /// Pull the next non-control message, mapping `Close`/`None` to
    /// [`WsError::Disconnected`] and `Ping`/`Pong` straight back to the caller
    /// (the `recv_*` loops decide whether to skip them).
    async fn next_message(&mut self) -> Result<Message, WsError> {
        match self.socket.recv().await {
            Some(Ok(Message::Close(_))) | None => Err(WsError::Disconnected),
            Some(Ok(msg)) => Ok(msg),
            Some(Err(e)) => Err(WsError::Transport(e.to_string())),
        }
    }

    /// Send a close frame with the given status `code` and optional `reason`,
    /// then mark the session closed. Idempotent: a second call is a no-op.
    ///
    /// `code` follows the RFC 6455 close-code registry (1000 = normal). This
    /// mirrors pyfly's `close(code=1000, reason=None)`.
    pub async fn close(&mut self, code: u16, reason: Option<String>) -> Result<(), WsError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        let frame = CloseFrame {
            code,
            reason: reason.unwrap_or_default().into(),
        };
        self.socket
            .send(Message::Close(Some(frame)))
            .await
            .map_err(|e| WsError::Transport(e.to_string()))
    }
}
