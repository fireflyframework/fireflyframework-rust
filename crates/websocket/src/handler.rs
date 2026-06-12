//! [`WebSocketHandler`] trait plus the [`serve_ws`] / [`ws_route`] glue that
//! drives a handler over an upgraded axum socket.

use std::sync::Arc;

use async_trait::async_trait;
use axum::{
    extract::ws::WebSocketUpgrade,
    response::Response,
    routing::{get, MethodRouter},
};

use crate::error::WsError;
use crate::session::WsSession;

/// Lifecycle hooks a WebSocket controller implements.
///
/// This is the Rust counterpart of pyfly's `WebSocketHandler` protocol, and it
/// preserves the same contract:
///
/// * [`handle`](WebSocketHandler::handle) owns the **full** lifecycle — it runs
///   the receive loop and decides when to send, just like a pyfly
///   `@websocket_mapping` method. It is the only method the framework dispatches
///   per connection.
/// * [`on_connect`](WebSocketHandler::on_connect) and
///   [`on_message`](WebSocketHandler::on_message) are **convenience** hooks that
///   are **never auto-dispatched** by the framework (locked in by pyfly's
///   `test_on_message_is_not_auto_dispatched`). Call them yourself from
///   `handle` if you want them.
/// * [`on_disconnect`](WebSocketHandler::on_disconnect) is invoked
///   **automatically** by [`serve_ws`] after `handle` returns or the socket
///   closes. Because axum only runs the upgrade callback *after* a successful
///   handshake, the "only if accepted" gate from pyfly is satisfied
///   structurally — there is no never-accepted path to guard against.
///
/// The default implementations of `on_connect`, `on_message`, and
/// `on_disconnect` are no-ops, so a minimal handler need only implement
/// `handle`.
///
/// A handler is a process-wide singleton shared across **all** connections
/// (it is held in an [`Arc`]). Keep per-connection state in local variables or
/// on the [`WsSession`] metadata, never on `self`.
#[async_trait]
pub trait WebSocketHandler: Send + Sync + 'static {
    /// Drive the connection: run the receive loop, send replies, and return
    /// when done. This is the only method the framework dispatches.
    ///
    /// Returning [`WsError::Disconnected`] (the normal end of a `recv_*` loop)
    /// is treated as a clean close, not a failure.
    async fn handle(&self, session: &mut WsSession) -> Result<(), WsError>;

    /// Convenience hook — **not** auto-called. Invoke it yourself from
    /// [`handle`](WebSocketHandler::handle) if you want connect-time logic.
    async fn on_connect(&self, _session: &mut WsSession) -> Result<(), WsError> {
        Ok(())
    }

    /// Convenience hook — **not** auto-called. The framework does not dispatch
    /// incoming messages; run your own receive loop in
    /// [`handle`](WebSocketHandler::handle) and call this if you want.
    async fn on_message(&self, _session: &mut WsSession, _data: String) -> Result<(), WsError> {
        Ok(())
    }

    /// Called automatically by [`serve_ws`] after `handle` returns or the
    /// connection closes. Use it for cleanup (e.g. leaving a
    /// [`BroadcastHub`](crate::BroadcastHub) topic). Failures here are logged,
    /// not propagated.
    async fn on_disconnect(&self, _session: &mut WsSession) -> Result<(), WsError> {
        Ok(())
    }
}

/// Drive `handler` over an upgraded WebSocket and return the upgrade response.
///
/// This is the core of the crate: it performs `upgrade.on_upgrade(...)`, wraps
/// the resulting socket in a [`WsSession`], runs
/// [`handle`](WebSocketHandler::handle), and **always** invokes
/// [`on_disconnect`](WebSocketHandler::on_disconnect) afterward — mirroring the
/// pyfly registrar's `finally`-block cleanup contract.
///
/// Error handling matches pyfly: a [`WsError::Disconnected`] from `handle` is a
/// clean close and is swallowed; any other error is logged via
/// [`tracing::warn`] rather than swallowed silently; and an error from
/// `on_disconnect` is likewise logged, never propagated.
///
/// Call this from an axum handler that has already extracted the upgrade (and
/// any `Path`/`Query`/`HeaderMap`/`protocols` you need):
///
/// ```no_run
/// use std::sync::Arc;
/// use axum::{extract::ws::WebSocketUpgrade, response::Response};
/// use firefly_websocket::{serve_ws, WebSocketHandler};
/// # struct Echo;
/// # #[async_trait::async_trait]
/// # impl WebSocketHandler for Echo {
/// #     async fn handle(&self, s: &mut firefly_websocket::WsSession) -> Result<(), firefly_websocket::WsError> {
/// #         loop { let m = s.recv_text().await?; s.send_text(m).await?; }
/// #     }
/// # }
///
/// async fn echo(ws: WebSocketUpgrade) -> Response {
///     serve_ws(ws, Arc::new(Echo))
/// }
/// ```
pub fn serve_ws<H: WebSocketHandler>(upgrade: WebSocketUpgrade, handler: Arc<H>) -> Response {
    upgrade.on_upgrade(move |socket| async move {
        let mut session = WsSession::new(socket);
        match handler.handle(&mut session).await {
            Ok(()) | Err(WsError::Disconnected) => {}
            Err(e) => {
                // Unexpected handler failures are logged rather than swallowed
                // silently — mirrors pyfly audit #232.
                tracing::warn!(error = %e, "websocket handler returned an error");
            }
        }
        // Always run cleanup; axum only reaches here post-handshake, so the
        // "only if accepted" gate is structural. Log (not swallow) failures so
        // leaked resources surface.
        if let Err(e) = handler.on_disconnect(&mut session).await {
            tracing::warn!(error = %e, "websocket on_disconnect returned an error");
        }
    })
}

/// Build a `GET` [`MethodRouter`] that upgrades to WebSocket and drives
/// `handler`, ready to mount with [`axum::Router::route`].
///
/// This is the explicit-registration replacement for pyfly's
/// `@websocket_mapping` decorator + DI-scanning `WebSocketRegistrar`. Wire it
/// up like any other axum route:
///
/// ```no_run
/// use std::sync::Arc;
/// use axum::Router;
/// use firefly_websocket::{ws_route, WebSocketHandler};
/// # struct Echo;
/// # #[async_trait::async_trait]
/// # impl WebSocketHandler for Echo {
/// #     async fn handle(&self, s: &mut firefly_websocket::WsSession) -> Result<(), firefly_websocket::WsError> {
/// #         loop { let m = s.recv_text().await?; s.send_text(m).await?; }
/// #     }
/// # }
///
/// let app: Router = Router::new().route("/ws/echo", ws_route(Arc::new(Echo)));
/// # let _ = app;
/// ```
///
/// To select a subprotocol or read path/query/headers, write the axum handler
/// by hand and call [`serve_ws`] instead.
pub fn ws_route<H: WebSocketHandler>(handler: Arc<H>) -> MethodRouter {
    get(move |upgrade: WebSocketUpgrade| {
        let handler = handler.clone();
        async move { serve_ws(upgrade, handler) }
    })
}
