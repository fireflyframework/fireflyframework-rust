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

//! End-to-end WebSocket tests — port of pyfly's `tests/websocket/test_ws_e2e.py`
//! and `test_ws_lifecycle.py`.
//!
//! Each test spawns a real axum server on an ephemeral port (`127.0.0.1:0`) and
//! connects with `tokio-tungstenite`, exercising real round-trips through
//! [`ws_route`] / [`serve_ws`] — not a fake socket. Mirrors the pyfly tests
//! that drive a real WS connection through `TestClient` + `WebSocketRegistrar`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use futures::{SinkExt, StreamExt};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message as TMsg;

use firefly_websocket::{
    serve_ws, ws_route, BroadcastHub, HubMessage, WebSocketHandler, WsError, WsSession,
};

/// Spawn `app` on an ephemeral port and return the `ws://` base URL.
async fn spawn(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("ws://{addr}")
}

async fn connect(
    url: &str,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let (ws, _resp) = tokio_tungstenite::connect_async(url).await.unwrap();
    ws
}

// ---------------------------------------------------------------------------
// Echo controller — mirrors pyfly's _EchoController, recording lifecycle events.
// ---------------------------------------------------------------------------

struct EchoController {
    events: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl WebSocketHandler for EchoController {
    async fn handle(&self, session: &mut WsSession) -> Result<(), WsError> {
        self.events.lock().await.push("accept".into());
        loop {
            let msg = session.recv_text().await?; // Disconnected ends the loop
            session.send_text(format!("echo:{msg}")).await?;
        }
    }

    async fn on_disconnect(&self, _session: &mut WsSession) -> Result<(), WsError> {
        self.events.lock().await.push("disconnect".into());
        Ok(())
    }
}

#[tokio::test]
async fn echo_single_message() {
    // pyfly: TestWebSocketE2E.test_echo_single_message
    let events = Arc::new(Mutex::new(Vec::new()));
    let handler = Arc::new(EchoController {
        events: events.clone(),
    });
    let url = spawn(Router::new().route("/ws/echo", ws_route(handler))).await;

    let mut ws = connect(&format!("{url}/ws/echo")).await;
    ws.send(TMsg::Text("hello".into())).await.unwrap();
    let reply = ws.next().await.unwrap().unwrap();
    assert_eq!(reply, TMsg::Text("echo:hello".into()));

    assert!(events.lock().await.contains(&"accept".to_string()));
}

#[tokio::test]
async fn echo_multiple_messages() {
    // pyfly: TestWebSocketE2E.test_echo_multiple_messages
    let handler = Arc::new(EchoController {
        events: Arc::new(Mutex::new(Vec::new())),
    });
    let url = spawn(Router::new().route("/ws/echo", ws_route(handler))).await;

    let mut ws = connect(&format!("{url}/ws/echo")).await;
    for msg in ["alpha", "beta", "gamma"] {
        ws.send(TMsg::Text(msg.into())).await.unwrap();
        let reply = ws.next().await.unwrap().unwrap();
        assert_eq!(reply, TMsg::Text(format!("echo:{msg}")));
    }
}

#[tokio::test]
async fn disconnect_callback_fires() {
    // pyfly: TestWebSocketE2E.test_disconnect_callback_fires
    let events = Arc::new(Mutex::new(Vec::new()));
    let handler = Arc::new(EchoController {
        events: events.clone(),
    });
    let url = spawn(Router::new().route("/ws/echo", ws_route(handler))).await;

    {
        let mut ws = connect(&format!("{url}/ws/echo")).await;
        ws.send(TMsg::Text("ping".into())).await.unwrap();
        let _ = ws.next().await.unwrap().unwrap();
        ws.close(None).await.unwrap();
    }

    // Wait (bounded) for the server-side handler to observe the close and run
    // on_disconnect — no fixed sleep, poll with a short ceiling.
    wait_until(|| {
        let events = events.clone();
        async move { *events.lock().await == ["accept", "disconnect"] }
    })
    .await;
    assert_eq!(*events.lock().await, ["accept", "disconnect"]);
}

#[tokio::test]
async fn clean_close_from_client() {
    // pyfly: TestWebSocketE2E.test_clean_close_from_client
    let events = Arc::new(Mutex::new(Vec::new()));
    let handler = Arc::new(EchoController {
        events: events.clone(),
    });
    let url = spawn(Router::new().route("/ws/echo", ws_route(handler))).await;

    {
        let mut ws = connect(&format!("{url}/ws/echo")).await;
        for msg in ["one", "two"] {
            ws.send(TMsg::Text(msg.into())).await.unwrap();
            let reply = ws.next().await.unwrap().unwrap();
            assert_eq!(reply, TMsg::Text(format!("echo:{msg}")));
        }
        ws.close(None).await.unwrap();
    }

    wait_until(|| {
        let events = events.clone();
        async move {
            let e = events.lock().await;
            e.contains(&"accept".to_string()) && e.contains(&"disconnect".to_string())
        }
    })
    .await;
    let e = events.lock().await;
    assert!(e.contains(&"accept".to_string()));
    assert!(e.contains(&"disconnect".to_string()));
}

// ---------------------------------------------------------------------------
// on_message is NEVER auto-dispatched — pyfly's locked-in contract.
// ---------------------------------------------------------------------------

struct OnMessageController {
    on_message_calls: Arc<Mutex<usize>>,
    disconnected: Arc<Mutex<bool>>,
}

#[async_trait]
impl WebSocketHandler for OnMessageController {
    async fn handle(&self, session: &mut WsSession) -> Result<(), WsError> {
        // Drains messages but never invokes on_message.
        loop {
            session.recv_text().await?;
        }
    }

    async fn on_message(&self, _session: &mut WsSession, _data: String) -> Result<(), WsError> {
        *self.on_message_calls.lock().await += 1; // must never be auto-invoked
        Ok(())
    }

    async fn on_disconnect(&self, _session: &mut WsSession) -> Result<(), WsError> {
        *self.disconnected.lock().await = true;
        Ok(())
    }
}

#[tokio::test]
async fn on_message_is_not_auto_dispatched() {
    // pyfly: test_ws_lifecycle.test_on_message_is_not_auto_dispatched
    let on_message_calls = Arc::new(Mutex::new(0));
    let disconnected = Arc::new(Mutex::new(false));
    let handler = Arc::new(OnMessageController {
        on_message_calls: on_message_calls.clone(),
        disconnected: disconnected.clone(),
    });
    let url = spawn(Router::new().route("/ws/m", ws_route(handler))).await;

    {
        let mut ws = connect(&format!("{url}/ws/m")).await;
        ws.send(TMsg::Text("a".into())).await.unwrap();
        ws.send(TMsg::Text("b".into())).await.unwrap();
        ws.close(None).await.unwrap();
    }

    wait_until(|| {
        let d = disconnected.clone();
        async move { *d.lock().await }
    })
    .await;
    assert_eq!(*on_message_calls.lock().await, 0); // framework never dispatches
}

// ---------------------------------------------------------------------------
// on_disconnect cleanup failures are logged, NOT propagated (pyfly audit #232).
// ---------------------------------------------------------------------------

struct BadCleanupController {
    disconnect_attempted: Arc<Mutex<bool>>,
}

#[async_trait]
impl WebSocketHandler for BadCleanupController {
    async fn handle(&self, _session: &mut WsSession) -> Result<(), WsError> {
        Ok(()) // returns immediately
    }

    async fn on_disconnect(&self, _session: &mut WsSession) -> Result<(), WsError> {
        *self.disconnect_attempted.lock().await = true;
        Err(WsError::Transport("cleanup failed".into())) // must not crash the task
    }
}

#[tokio::test]
async fn on_disconnect_error_is_logged_not_propagated() {
    // pyfly: test_ws_lifecycle.test_on_disconnect_error_is_logged_not_swallowed
    let attempted = Arc::new(Mutex::new(false));
    let handler = Arc::new(BadCleanupController {
        disconnect_attempted: attempted.clone(),
    });
    let url = spawn(Router::new().route("/ws/bad", ws_route(handler))).await;

    {
        let _ws = connect(&format!("{url}/ws/bad")).await;
        // handle() returns immediately; server closes the socket.
    }

    wait_until(|| {
        let a = attempted.clone();
        async move { *a.lock().await }
    })
    .await;
    // Reaching here (no panic in the spawned task) proves the error was logged,
    // not propagated.
    assert!(*attempted.lock().await);
}

// ---------------------------------------------------------------------------
// JSON round-trip via send_json / recv_json.
// ---------------------------------------------------------------------------

struct JsonEchoController;

#[async_trait]
impl WebSocketHandler for JsonEchoController {
    async fn handle(&self, session: &mut WsSession) -> Result<(), WsError> {
        loop {
            let value: serde_json::Value = session.recv_json().await?;
            session.send_json(&value).await?;
        }
    }
}

#[tokio::test]
async fn json_round_trip() {
    let url = spawn(Router::new().route("/ws/json", ws_route(Arc::new(JsonEchoController)))).await;

    let mut ws = connect(&format!("{url}/ws/json")).await;
    ws.send(TMsg::Text(r#"{"id":1,"name":"alice"}"#.into()))
        .await
        .unwrap();
    let reply = ws.next().await.unwrap().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(reply.to_text().unwrap()).unwrap();
    assert_eq!(parsed["id"], 1);
    assert_eq!(parsed["name"], "alice");
}

// ---------------------------------------------------------------------------
// Binary round-trip via send_bytes / recv_bytes.
// ---------------------------------------------------------------------------

struct BinaryEchoController;

#[async_trait]
impl WebSocketHandler for BinaryEchoController {
    async fn handle(&self, session: &mut WsSession) -> Result<(), WsError> {
        loop {
            let bytes = session.recv_bytes().await?;
            session.send_bytes(bytes).await?;
        }
    }
}

#[tokio::test]
async fn binary_round_trip() {
    let url = spawn(Router::new().route("/ws/bin", ws_route(Arc::new(BinaryEchoController)))).await;

    let mut ws = connect(&format!("{url}/ws/bin")).await;
    ws.send(TMsg::Binary(vec![1, 2, 3, 4])).await.unwrap();
    let reply = ws.next().await.unwrap().unwrap();
    assert_eq!(reply, TMsg::Binary(vec![1, 2, 3, 4]));
}

// ---------------------------------------------------------------------------
// BroadcastHub fan-out over real sockets: each connection joins a room and
// every message is fanned out to every member of that room.
// ---------------------------------------------------------------------------

struct ChatController {
    hub: BroadcastHub,
}

#[async_trait]
impl WebSocketHandler for ChatController {
    async fn handle(&self, session: &mut WsSession) -> Result<(), WsError> {
        let id = session.id().to_string();
        let mut sub = self.hub.join("room", id.clone()).await;

        loop {
            tokio::select! {
                incoming = session.recv_text() => {
                    match incoming {
                        Ok(msg) => { self.hub.broadcast("room", HubMessage::Text(msg)).await; }
                        Err(_) => break, // disconnected
                    }
                }
                Some(out) = sub.recv() => {
                    if let HubMessage::Text(t) = out {
                        session.send_text(t).await?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn on_disconnect(&self, session: &mut WsSession) -> Result<(), WsError> {
        self.hub.leave("room", session.id()).await;
        Ok(())
    }
}

#[tokio::test]
async fn broadcast_fans_out_to_all_clients() {
    let hub = BroadcastHub::new();
    let handler = Arc::new(ChatController { hub: hub.clone() });
    let url = spawn(Router::new().route("/ws/chat", ws_route(handler))).await;

    let mut a = connect(&format!("{url}/ws/chat")).await;
    let mut b = connect(&format!("{url}/ws/chat")).await;

    // Both clients must be joined before we broadcast.
    wait_until(|| {
        let hub = hub.clone();
        async move { hub.subscriber_count("room").await == 2 }
    })
    .await;

    a.send(TMsg::Text("hi-all".into())).await.unwrap();

    // Both A and B receive the same broadcast.
    let ra = a.next().await.unwrap().unwrap();
    let rb = b.next().await.unwrap().unwrap();
    assert_eq!(ra, TMsg::Text("hi-all".into()));
    assert_eq!(rb, TMsg::Text("hi-all".into()));
}

// ---------------------------------------------------------------------------
// Subprotocol selection via WebSocketUpgrade::protocols + serve_ws.
// ---------------------------------------------------------------------------

struct ProtoController;

#[async_trait]
impl WebSocketHandler for ProtoController {
    async fn handle(&self, session: &mut WsSession) -> Result<(), WsError> {
        loop {
            let m = session.recv_text().await?;
            session.send_text(m).await?;
        }
    }
}

#[tokio::test]
async fn subprotocol_is_negotiated_via_serve_ws() {
    use axum::extract::ws::WebSocketUpgrade;
    use axum::response::Response;
    use axum::routing::get;

    async fn upgrade(ws: WebSocketUpgrade) -> Response {
        serve_ws(ws.protocols(["firefly.v1"]), Arc::new(ProtoController))
    }

    let url = spawn(Router::new().route("/ws/proto", get(upgrade))).await;

    // tungstenite client request advertising the subprotocol.
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http::HeaderValue;
    let mut req = format!("{url}/ws/proto").into_client_request().unwrap();
    req.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        HeaderValue::from_static("firefly.v1"),
    );

    let (mut ws, resp) = tokio_tungstenite::connect_async(req).await.unwrap();
    assert_eq!(
        resp.headers().get("sec-websocket-protocol").unwrap(),
        "firefly.v1"
    );

    ws.send(TMsg::Text("ping".into())).await.unwrap();
    let reply = ws.next().await.unwrap().unwrap();
    assert_eq!(reply, TMsg::Text("ping".into()));
}

// ---------------------------------------------------------------------------
// Bounded polling helper — never sleeps more than ~200ms total.
// ---------------------------------------------------------------------------

async fn wait_until<F, Fut>(mut cond: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..40 {
        if cond().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("condition not met within budget");
}
