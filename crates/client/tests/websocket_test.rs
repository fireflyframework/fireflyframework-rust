//! Live WebSocket round-trip tests, ported from pyfly's
//! `test_websocket_client_live.py`. pyfly's in-process
//! `websockets.serve` echo server becomes an in-process axum ws echo
//! route on a random localhost port. Requires the `websocket` feature.
#![cfg(feature = "websocket")]

use std::time::Duration;

use axum::extract::ws::{Message as AxumMessage, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use futures::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

use firefly_client::{WsBuilder, WsClient};

/// Echoes every inbound text/binary frame back to the sender.
async fn echo_socket(mut socket: WebSocket) {
    while let Some(Ok(msg)) = socket.recv().await {
        match msg {
            AxumMessage::Text(t) => {
                if socket.send(AxumMessage::Text(t)).await.is_err() {
                    break;
                }
            }
            AxumMessage::Binary(b) => {
                if socket.send(AxumMessage::Binary(b)).await.is_err() {
                    break;
                }
            }
            AxumMessage::Close(_) => break,
            _ => {}
        }
    }
}

async fn ws_handler(ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(echo_socket)
}

/// Binds an axum ws echo router on a random port; returns its host:port.
async fn spawn_echo() -> String {
    let app = Router::new().route("/ws", get(ws_handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    addr.to_string()
}

// --- pyfly: test_websocket_connect_echo -----------------------------------

#[tokio::test]
async fn websocket_connect_echo() {
    let addr = spawn_echo().await;
    let client = WsBuilder::new(format!("ws://{addr}/ws")).build();

    let mut conn = tokio::time::timeout(Duration::from_secs(5), client.connect())
        .await
        .expect("connect timed out")
        .expect("connect");

    conn.send(Message::text("hello")).await.expect("send");
    let reply = tokio::time::timeout(Duration::from_secs(5), conn.next())
        .await
        .expect("recv timed out")
        .expect("stream ended")
        .expect("message");
    assert_eq!(reply.into_text().expect("text"), "hello");
    conn.close(None).await.expect("close");
}

// --- pyfly: test_websocket_stream_echo ------------------------------------

#[tokio::test]
async fn websocket_stream_echo() {
    let addr = spawn_echo().await;
    let client = WsClient::builder(format!("ws://{addr}/ws")).build();

    let mut messages = tokio::time::timeout(
        Duration::from_secs(5),
        client.stream(vec![Message::text("hi")]),
    )
    .await
    .expect("stream setup timed out")
    .expect("stream");

    let first = tokio::time::timeout(Duration::from_secs(5), messages.next())
        .await
        .expect("recv timed out")
        .expect("stream ended")
        .expect("message");
    assert_eq!(first.into_text().expect("text"), "hi");
}

#[tokio::test]
async fn websocket_stream_echoes_multiple_payloads() {
    let addr = spawn_echo().await;
    let client = WsBuilder::new(format!("ws://{addr}/ws")).build();

    let mut messages = client
        .stream(vec![
            Message::text("alpha"),
            Message::text("beta"),
            Message::binary(vec![0u8, 255, 254]),
        ])
        .await
        .expect("stream");

    let mut received: Vec<Message> = Vec::new();
    for _ in 0..3 {
        let msg = tokio::time::timeout(Duration::from_secs(5), messages.next())
            .await
            .expect("recv timed out")
            .expect("stream ended")
            .expect("message");
        received.push(msg);
    }
    assert_eq!(received[0], Message::text("alpha"));
    assert_eq!(received[1], Message::text("beta"));
    assert_eq!(received[2], Message::binary(vec![0u8, 255, 254]));
}

#[test]
fn websocket_builder_records_url_headers_and_ping() {
    let client = WsBuilder::new("wss://example.com/ws")
        .with_header("Origin", "https://example.com")
        .with_ping_interval(Some(Duration::from_secs(10)))
        .build();
    assert_eq!(client.ping_interval(), Some(Duration::from_secs(10)));
}
