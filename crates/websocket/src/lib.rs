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

//! # firefly-websocket
//!
//! **WebSocket server support over [axum](https://docs.rs/axum)** — the
//! full-duplex companion to [`firefly-sse`](https://docs.rs/firefly-sse)'s
//! half-duplex stream. The Rust port of pyfly's `pyfly.websocket` package.
//!
//! It provides:
//!
//! * [`WsSession`] — a typed handle over an accepted socket: `send_text` /
//!   `send_json` / `send_bytes`, `recv_text` / `recv_json` / `recv_bytes`,
//!   `close`, a stable [`id`](WsSession::id), and a free-form
//!   [`metadata`](WsSession::metadata) map.
//! * [`WebSocketHandler`] — the lifecycle trait controllers implement:
//!   `handle` (owns the receive loop), `on_connect` / `on_message`
//!   (convenience, never auto-dispatched), and `on_disconnect` (auto-invoked
//!   after `handle` returns).
//! * [`serve_ws`] / [`ws_route`] — the glue that upgrades a connection, drives
//!   a handler, and guarantees `on_disconnect` runs.
//! * [`BroadcastHub`] — topic-based fan-out (`join` / `leave` / `broadcast`).
//! * [`WsError`] / [`HubMessage`] — the supporting types.
//!
//! ## Adaptation from pyfly
//!
//! pyfly is decorator-driven: a `@websocket_mapping` method on a
//! `@rest_controller` bean, discovered by a DI-scanning `WebSocketRegistrar`.
//! Rust registers routes **explicitly** on an [`axum::Router`], consistent with
//! the starter-core `apply_middleware` pattern, so the decorator and registrar
//! collapse into [`ws_route`]. pyfly's `WebSocketSession.accept()` and
//! `accepted` flag are gone: axum runs the upgrade callback only **after** the
//! handshake, so a [`WsSession`] is always already accepted and the "fire
//! `on_disconnect` only if accepted" gate is satisfied structurally. The
//! lifecycle contract — `handle` owns the loop, `on_message`/`on_connect` are
//! never auto-dispatched, `on_disconnect` always runs once afterward with its
//! failures *logged* rather than swallowed — is preserved exactly (see
//! [`serve_ws`]).
//!
//! ## Quick start
//!
//! ```no_run
//! use std::sync::Arc;
//! use async_trait::async_trait;
//! use axum::Router;
//! use firefly_websocket::{ws_route, WebSocketHandler, WsError, WsSession};
//!
//! struct Echo;
//!
//! #[async_trait]
//! impl WebSocketHandler for Echo {
//!     async fn handle(&self, session: &mut WsSession) -> Result<(), WsError> {
//!         loop {
//!             let msg = session.recv_text().await?; // Err(Disconnected) ends the loop
//!             session.send_text(format!("echo:{msg}")).await?;
//!         }
//!     }
//! }
//!
//! let app: Router = Router::new().route("/ws/echo", ws_route(Arc::new(Echo)));
//! # let _ = app;
//! ```

mod error;
mod handler;
mod hub;
mod session;

pub use error::WsError;
pub use handler::{serve_ws, ws_route, WebSocketHandler};
pub use hub::{BroadcastHub, HubMessage, Subscription};
pub use session::WsSession;

/// Released framework version. Calendar-versioned (`YY.M.PATCH`), the Rust
/// port's counterpart of the Go `kernel.Version` constant.
pub const VERSION: &str = "26.6.12";

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync>() {}
    fn assert_send<T: Send>() {}

    #[test]
    fn public_types_are_send_and_sync() {
        // `WsSession` wraps an axum `WebSocket` (a `hyper::Upgraded`), which is
        // `Send` but not `Sync` — fine, a session is owned by a single task and
        // never shared across threads concurrently. `Send` is what lets it
        // cross an `await` / `tokio::spawn`.
        assert_send::<WsSession>();
        assert_send_sync::<WsError>();
        assert_send_sync::<BroadcastHub>();
        assert_send_sync::<HubMessage>();
    }

    #[test]
    fn version_matches_crate_version() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }
}
