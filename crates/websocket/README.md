# `firefly-websocket`

> **Tier:** Platform · **Status:** Stable · **Built on:** `axum` (`ws` feature)

## Overview

`firefly-websocket` provides **WebSocket server support over axum** — the
full-duplex companion to [`firefly-sse`](../sse)'s half-duplex stream. It gives
you a typed session handle, a lifecycle handler trait, explicit route
registration, and a topic-based broadcast hub.

```rust
use std::sync::Arc;
use async_trait::async_trait;
use axum::Router;
use firefly_websocket::{ws_route, WebSocketHandler, WsError, WsSession};

struct Echo;

#[async_trait]
impl WebSocketHandler for Echo {
    async fn handle(&self, session: &mut WsSession) -> Result<(), WsError> {
        loop {
            let msg = session.recv_text().await?; // Err(Disconnected) ends the loop
            session.send_text(format!("echo:{msg}")).await?;
        }
    }
}

let app: Router = Router::new().route("/ws/echo", ws_route(Arc::new(Echo)));
```

## Public surface

```rust,ignore
// A typed handle over an accepted socket.
pub struct WsSession { /* … */ }
impl WsSession {
    pub fn new(socket: axum::extract::ws::WebSocket) -> WsSession;
    pub fn id(&self) -> &str;                 // stable UUID v4
    pub fn metadata(&self) -> &HashMap<String, String>;
    pub fn metadata_mut(&mut self) -> &mut HashMap<String, String>;
    pub fn is_closed(&self) -> bool;
    pub async fn send_text(&mut self, data: impl Into<String>) -> Result<(), WsError>;
    pub async fn send_json<T: Serialize>(&mut self, value: &T) -> Result<(), WsError>;
    pub async fn send_bytes(&mut self, data: impl Into<Vec<u8>>) -> Result<(), WsError>;
    pub async fn recv_text(&mut self) -> Result<String, WsError>;
    pub async fn recv_json<T: DeserializeOwned>(&mut self) -> Result<T, WsError>;
    pub async fn recv_bytes(&mut self) -> Result<Vec<u8>, WsError>;
    pub async fn close(&mut self, code: u16, reason: Option<String>) -> Result<(), WsError>;
}

// The lifecycle trait controllers implement.
#[async_trait]
pub trait WebSocketHandler: Send + Sync + 'static {
    async fn handle(&self, session: &mut WsSession) -> Result<(), WsError>;       // owns the loop
    async fn on_connect(&self, session: &mut WsSession) -> Result<(), WsError>;    // convenience — never auto-called
    async fn on_message(&self, session: &mut WsSession, data: String) -> Result<(), WsError>; // convenience — never auto-called
    async fn on_disconnect(&self, session: &mut WsSession) -> Result<(), WsError>; // auto-called after handle returns
}

// Route glue — explicit registration on an axum::Router.
pub fn serve_ws<H: WebSocketHandler>(upgrade: WebSocketUpgrade, handler: Arc<H>) -> Response;
pub fn ws_route<H: WebSocketHandler>(handler: Arc<H>) -> MethodRouter;

// Topic-based fan-out.
pub struct BroadcastHub { /* … */ } // Clone (shared state), Default
impl BroadcastHub {
    pub fn new() -> BroadcastHub;
    pub async fn join(&self, topic: impl Into<String>, session_id: impl Into<String>) -> Subscription;
    pub async fn leave(&self, topic: &str, session_id: &str);
    pub async fn broadcast(&self, topic: &str, message: HubMessage) -> usize; // returns #delivered
    pub async fn subscriber_count(&self, topic: &str) -> usize;
    pub async fn topic_count(&self) -> usize;
}
pub type Subscription = tokio::sync::mpsc::UnboundedReceiver<HubMessage>;
pub enum HubMessage { Text(String), Binary(Vec<u8>) }

pub enum WsError { Disconnected, Protocol(String), Transport(String), Serde(serde_json::Error) }
```

## Lifecycle contract

The contract is locked in by `tests/e2e.rs`:

* **`handle` owns the full lifecycle.** It runs the receive loop and decides
  when to send — the framework dispatches only this method per connection.
* **`on_connect` / `on_message` are never auto-dispatched.** They are
  convenience hooks you may call yourself from `handle`. The framework does
  *not* push incoming messages to `on_message`
  (`on_message_is_not_auto_dispatched`).
* **`on_disconnect` always runs once**, after `handle` returns or the socket
  closes (`disconnect_callback_fires`, `clean_close_from_client`).
* **Cleanup failures are logged, not swallowed and not propagated.** An error
  from `on_disconnect` is logged via `tracing::warn!` and the task ends cleanly
  (`on_disconnect_error_is_logged_not_propagated`). An unexpected error returned
  from `handle` is likewise logged at warn; a `WsError::Disconnected` is treated
  as a clean close and swallowed.

## Route registration & axum integration

Routes are wired explicitly on an `axum::Router`, consistent with starter-core's
`apply_middleware` pattern — `Router::new().route("/ws/echo", ws_route(Arc::new(handler)))`.
There is no separate handshake step: axum runs the upgrade callback only after
the handshake completes, so a `WsSession` is always already accepted. That also
means `on_disconnect` is gated **structurally** — `serve_ws`'s closure only runs
after a successful handshake.

To read request context at the route layer, use axum extractors (`Path` /
`Query` / `HeaderMap`), write the handler by hand, and call `serve_ws`. Select
subprotocols with `WebSocketUpgrade::protocols([...])` before handing off to
`serve_ws`. A peer disconnect surfaces as `WsError::Disconnected` returned from
the `recv_*` methods.

The `BroadcastHub` covers the common chat/presence fan-out pattern, keyed by
`WsSession::id`.

## Testing

```bash
cargo test -p firefly-websocket
```

`tests/e2e.rs` spawns a real axum server on an ephemeral port
(`127.0.0.1:0`) and connects with `tokio-tungstenite` (a dev-dependency),
exercising real round-trips through `ws_route` / `serve_ws` — not a fake
socket. It covers every websocket scenario (echo single/multiple, the
disconnect callback, clean client close, the `on_message`-never-dispatched
contract, and logged-not-propagated cleanup failures) plus JSON/binary
round-trips, subprotocol negotiation, and `BroadcastHub` fan-out across two
live clients. `BroadcastHub` join/leave/broadcast/prune logic is unit-tested in
`src/hub.rs` with no sockets. No test sleeps more than ~200 ms (lifecycle waits
poll a condition with a 5 ms tick and a 200 ms ceiling).
