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

//! WebSocket client over [`tokio_tungstenite`] — feature `websocket`.
//!
//! The Rust port of pyfly's `WebSocketClient`: a thin helper that opens
//! a connection (with optional custom request headers), and a
//! [`WsClient::stream`] convenience that sends an initial burst of
//! messages and then hands back the inbound message stream.
//!
//! Unlike pyfly — which wraps the `websockets` library's ping
//! keep-alive — `tokio-tungstenite` performs protocol-level Ping/Pong
//! transparently, so [`WsBuilder::with_ping_interval`] is accepted for
//! API symmetry and recorded on the client, but the heartbeat itself is
//! handled by the underlying stream. The escape hatch for full control
//! is [`WsClient::connect`], which returns the raw
//! [`tokio_tungstenite::WebSocketStream`].

use std::time::Duration;

use futures::stream::{Stream, StreamExt};
use futures::SinkExt;
use http::header::{HeaderName, HeaderValue};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

/// The connected stream type returned by [`WsClient::connect`] — a
/// `tokio-tungstenite` [`WebSocketStream`] over a (possibly TLS-wrapped)
/// TCP stream. Implements both [`Stream`] of inbound [`Message`]s and
/// [`futures::Sink`] for outbound ones.
pub type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Default ping interval (20 s), matching pyfly's `WebSocketClient`.
const DEFAULT_PING_INTERVAL: Duration = Duration::from_secs(20);

/// Fluently configures a [`WsClient`] — the Rust analog of pyfly's
/// `WebSocketClientBuilder`.
///
/// ```
/// use firefly_client::WsBuilder;
///
/// let client = WsBuilder::new("wss://example.com/ws")
///     .with_header("Origin", "https://example.com")
///     .build();
/// # let _ = client;
/// ```
#[derive(Debug, Clone)]
pub struct WsBuilder {
    url: String,
    headers: Vec<(String, String)>,
    ping_interval: Option<Duration>,
}

impl WsBuilder {
    /// Returns a builder primed for the given WebSocket URL
    /// (`ws://` or `wss://`).
    pub fn new(url: impl AsRef<str>) -> Self {
        Self {
            url: url.as_ref().to_owned(),
            headers: Vec::new(),
            ping_interval: Some(DEFAULT_PING_INTERVAL),
        }
    }

    /// Adds a custom handshake request header (pyfly's `with_header`),
    /// e.g. `Authorization` or `Origin`. Order is preserved.
    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Sets the keep-alive ping interval (default 20 s). Recorded for
    /// parity with pyfly; the underlying transport drives Ping/Pong.
    /// `None` disables the recorded interval.
    #[must_use]
    pub fn with_ping_interval(mut self, interval: Option<Duration>) -> Self {
        self.ping_interval = interval;
        self
    }

    /// Finalises the [`WsClient`].
    pub fn build(self) -> WsClient {
        WsClient {
            url: self.url,
            headers: self.headers,
            ping_interval: self.ping_interval,
        }
    }
}

/// A WebSocket client built by [`WsBuilder`] — the Rust analog of
/// pyfly's `WebSocketClient`.
#[derive(Debug, Clone)]
pub struct WsClient {
    url: String,
    headers: Vec<(String, String)>,
    ping_interval: Option<Duration>,
}

impl WsClient {
    /// Returns a builder primed for the given URL.
    pub fn builder(url: impl AsRef<str>) -> WsBuilder {
        WsBuilder::new(url)
    }

    /// The configured keep-alive ping interval, if any.
    #[must_use]
    pub fn ping_interval(&self) -> Option<Duration> {
        self.ping_interval
    }

    /// Opens the connection and returns the live [`WsStream`] — the Rust
    /// analog of pyfly's `connect()`. The caller drives send/receive
    /// directly via the [`futures::Sink`] / [`Stream`] impls.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`tokio_tungstenite::tungstenite::Error`]
    /// when the URL is malformed or the handshake fails.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
    /// use firefly_client::WsBuilder;
    /// use futures::{SinkExt, StreamExt};
    /// use tokio_tungstenite::tungstenite::Message;
    ///
    /// let client = WsBuilder::new("ws://127.0.0.1:9001").build();
    /// let mut conn = client.connect().await?;
    /// conn.send(Message::text("hello")).await?;
    /// if let Some(Ok(msg)) = conn.next().await {
    ///     println!("got {msg}");
    /// }
    /// conn.close(None).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn connect(&self) -> Result<WsStream, WsError> {
        let mut request = self.url.as_str().into_client_request()?;
        if !self.headers.is_empty() {
            let map = request.headers_mut();
            for (name, value) in &self.headers {
                let name = HeaderName::from_bytes(name.as_bytes()).map_err(|e| {
                    WsError::Url(
                        tokio_tungstenite::tungstenite::error::UrlError::UnableToConnect(
                            e.to_string(),
                        ),
                    )
                })?;
                let value = HeaderValue::from_str(value).map_err(|e| {
                    WsError::Url(
                        tokio_tungstenite::tungstenite::error::UrlError::UnableToConnect(
                            e.to_string(),
                        ),
                    )
                })?;
                map.append(name, value);
            }
        }
        let (stream, _response) = connect_async(request).await?;
        Ok(stream)
    }

    /// Connects, sends each message in `send` in order, then returns the
    /// inbound message [`Stream`] — the Rust analog of pyfly's
    /// `stream(send=[...])` async iterator.
    ///
    /// The returned stream owns the connection; dropping it closes the
    /// socket. Errors from the initial sends surface as the stream's
    /// first item.
    ///
    /// # Errors
    ///
    /// Returns the handshake error from [`WsClient::connect`] when the
    /// connection cannot be established.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
    /// use firefly_client::WsBuilder;
    /// use futures::StreamExt;
    /// use tokio_tungstenite::tungstenite::Message;
    ///
    /// let client = WsBuilder::new("ws://127.0.0.1:9001").build();
    /// let mut messages = client.stream(vec![Message::text("hi")]).await?;
    /// if let Some(Ok(echo)) = messages.next().await {
    ///     assert_eq!(echo.into_text()?, "hi");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn stream(
        &self,
        send: Vec<Message>,
    ) -> Result<impl Stream<Item = Result<Message, WsError>>, WsError> {
        let mut conn = self.connect().await?;
        let err = {
            let mut first_err = None;
            for msg in send {
                if let Err(e) = conn.send(msg).await {
                    first_err = Some(e);
                    break;
                }
            }
            first_err
        };
        // Prepend any send error as the stream's first yielded item so a
        // failed initial send is observable through the same channel,
        // then forward the live inbound stream.
        let prefix = futures::stream::iter(err.into_iter().map(Err));
        Ok(prefix.chain(conn))
    }
}
