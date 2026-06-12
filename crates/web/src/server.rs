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

//! Config-driven application-server bootstrap — the Rust port of
//! pyfly's `pyfly.server` layer (Spring Boot's `WebServer` analog).
//!
//! pyfly selects between Granian/Uvicorn/Hypercorn ASGI adapters; in
//! Rust there is one canonical hyper/axum/tokio stack, so the
//! port/adapter split collapses into a single [`Server`] built from a
//! [`ServerProperties`] (bound from configuration under `server.*`):
//!
//! * **TLS termination** ([`TlsConfig`], `axum-server` + rustls, ALPN
//!   `h2`/`http/1.1`) — pyfly's `ssl_certfile` / `ssl_keyfile`;
//! * **connection tuning** — `backlog` and `SO_REUSEADDR` via `socket2`,
//!   TCP keep-alive from `keep_alive_timeout`, in-flight request cap
//!   from `max_concurrent_connections`
//!   (`tower::limit::GlobalConcurrencyLimitLayer`);
//! * **graceful shutdown** — [`serve`] takes any shutdown future
//!   (designed for `firefly_lifecycle::ShutdownSignal::wait`) and
//!   drains within `graceful_timeout`;
//! * **[`ServerInfo`]** — a frozen runtime snapshot for startup logging
//!   and the `/actuator/info` contributor, mirroring pyfly's
//!   `ServerInfo` dataclass.
//!
//! Not ported (Python-runtime machinery): multi-process `workers` (use
//! tokio `worker_threads`), the `EventLoopPort` uvloop/winloop policy
//! (the tokio runtime), and adapter auto-configuration.
//!
//! ## Lifecycle wiring
//!
//! ```no_run
//! use axum::{routing::get, Router};
//! use firefly_lifecycle::Application;
//! use firefly_web::server::{serve, ServerProperties};
//!
//! # async fn run() -> Result<(), firefly_lifecycle::LifecycleError> {
//! let router = Router::new().route("/", get(|| async { "ok" }));
//! let props = ServerProperties::default();
//! Application::new("api")
//!     .on_server("http", move |shutdown| serve(router, props, shutdown.wait()))
//!     .run()
//!     .await
//! # }
//! ```

use std::future::Future;
use std::net::{SocketAddr, ToSocketAddrs};
use std::time::Duration;

use axum::Router;
use axum_server::tls_rustls::RustlsConfig;
use axum_server::Handle;
use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, Socket, TcpKeepalive, Type};
use tower::limit::GlobalConcurrencyLimitLayer;

/// Installs the `ring` rustls [`CryptoProvider`] as the process default,
/// exactly once. rustls 0.23 refuses to auto-select a provider when both
/// `ring` and `aws-lc-rs` are linked (they arrive transitively via lettre
/// and reqwest), so TLS termination would otherwise panic on first use.
/// Idempotent and race-safe: a second `install_default` simply returns
/// `Err`, which we ignore.
fn install_crypto_provider() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Boxed error type compatible with `firefly_lifecycle::HookResult`,
/// so [`serve`] drops straight into `Application::on_server`.
pub type ServeError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// TLS termination settings — pyfly's `ssl_certfile` / `ssl_keyfile`
/// pair. Both files must be PEM-encoded; ALPN advertises `h2` and
/// `http/1.1`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TlsConfig {
    /// Path to the PEM certificate (chain) file.
    #[serde(alias = "cert-file", alias = "ssl_certfile", alias = "ssl-certfile")]
    pub cert_file: String,
    /// Path to the PEM private-key file.
    #[serde(alias = "key-file", alias = "ssl_keyfile", alias = "ssl-keyfile")]
    pub key_file: String,
}

fn default_host() -> String {
    "0.0.0.0".to_string()
}

fn default_port() -> u16 {
    8000
}

fn default_backlog() -> u32 {
    1024
}

fn default_graceful_timeout() -> u64 {
    30
}

fn default_keep_alive_timeout() -> u64 {
    5
}

/// Configuration for the application server, deserialized from the
/// `server.*` configuration prefix — the Rust spelling of pyfly's
/// `ServerProperties` (`pyfly.server.*`), minus the Python-only
/// `workers` / `event_loop` / adapter-selection fields.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ServerProperties {
    /// Bind address. Default: `0.0.0.0`.
    pub host: String,
    /// Bind port; `0` asks the OS for an ephemeral port (use
    /// [`Server::local_addr`] to discover it). Default: `8000`.
    pub port: u16,
    /// Listen backlog. Default: `1024`.
    pub backlog: u32,
    /// Seconds the server may keep draining in-flight requests after
    /// the shutdown future resolves. Default: `30` (matching pyfly and
    /// the lifecycle drain budget).
    #[serde(alias = "graceful-timeout")]
    pub graceful_timeout: u64,
    /// Idle keep-alive seconds, applied as the TCP keep-alive time on
    /// the listening socket. Default: `5`. `0` disables it.
    #[serde(alias = "keep-alive-timeout")]
    pub keep_alive_timeout: u64,
    /// Cap on concurrently processed requests (a shared
    /// [`GlobalConcurrencyLimitLayer`]); `None` (default) means
    /// unlimited.
    #[serde(alias = "max-concurrent-connections")]
    pub max_concurrent_connections: Option<usize>,
    /// TLS termination; `None` (default) serves plain HTTP (with h2c).
    pub tls: Option<TlsConfig>,
}

impl Default for ServerProperties {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            backlog: default_backlog(),
            graceful_timeout: default_graceful_timeout(),
            keep_alive_timeout: default_keep_alive_timeout(),
            max_concurrent_connections: None,
            tls: None,
        }
    }
}

/// Immutable snapshot of application-server runtime information — the
/// Rust port of pyfly's frozen `ServerInfo` dataclass, consumed by
/// startup logging and the `/actuator/info` contributor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServerInfo {
    /// Server implementation name — always `hyper` (the single
    /// canonical Rust stack; pyfly reports granian/uvicorn/hypercorn).
    pub name: String,
    /// Framework version serving the stack.
    pub version: String,
    /// Bound host.
    pub host: String,
    /// Bound port (the *actual* port after binding port `0`).
    pub port: u16,
    /// Negotiated protocol surface — `auto` (HTTP/1.1 + h2/h2c),
    /// matching pyfly's default `http` setting.
    pub http_protocol: String,
    /// Whether TLS termination is enabled.
    pub tls: bool,
}

impl ServerInfo {
    /// Builds the snapshot from `properties` (before binding — the
    /// port is the configured one; prefer [`Server::info`] for the
    /// actual bound port).
    pub fn from_properties(properties: &ServerProperties) -> Self {
        Self {
            name: "hyper".to_string(),
            version: crate::VERSION.to_string(),
            host: properties.host.clone(),
            port: properties.port,
            http_protocol: "auto".to_string(),
            tls: properties.tls.is_some(),
        }
    }
}

/// A bound-but-not-yet-serving server — the builder returned by
/// [`Server::bind`]. Binding eagerly (separately from serving) lets
/// callers read [`Server::local_addr`] when configured with port `0`
/// and register [`Server::info`] with the actuator before traffic
/// flows.
#[derive(Debug)]
pub struct Server {
    properties: ServerProperties,
    listener: std::net::TcpListener,
    local_addr: SocketAddr,
}

fn resolve_addr(host: &str, port: u16) -> std::io::Result<SocketAddr> {
    (host, port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| std::io::Error::other(format!("cannot resolve bind address {host}:{port}")))
}

impl Server {
    /// Binds the listening socket described by `properties`, applying
    /// `SO_REUSEADDR`, the configured `backlog`, and TCP keep-alive.
    pub fn bind(properties: &ServerProperties) -> std::io::Result<Self> {
        let addr = resolve_addr(&properties.host, properties.port)?;
        let domain = if addr.is_ipv4() {
            Domain::IPV4
        } else {
            Domain::IPV6
        };
        let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
        socket.set_reuse_address(true)?;
        if properties.keep_alive_timeout > 0 {
            let keepalive =
                TcpKeepalive::new().with_time(Duration::from_secs(properties.keep_alive_timeout));
            socket.set_tcp_keepalive(&keepalive)?;
        }
        socket.bind(&addr.into())?;
        socket.listen(properties.backlog.min(i32::MAX as u32) as i32)?;
        socket.set_nonblocking(true)?;
        let listener: std::net::TcpListener = socket.into();
        let local_addr = listener.local_addr()?;
        Ok(Self {
            properties: properties.clone(),
            listener,
            local_addr,
        })
    }

    /// The actual bound address (resolves port `0`).
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// The runtime snapshot with the **actual** bound port.
    pub fn info(&self) -> ServerInfo {
        ServerInfo {
            port: self.local_addr.port(),
            ..ServerInfo::from_properties(&self.properties)
        }
    }

    /// Serves `router` until `shutdown` resolves, then drains in-flight
    /// requests within `graceful_timeout`. TLS (when configured) is
    /// terminated by rustls with ALPN `h2`/`http/1.1`; plain HTTP also
    /// speaks h2c.
    pub async fn serve<F>(self, router: Router, shutdown: F) -> Result<(), ServeError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let Self {
            properties,
            listener,
            ..
        } = self;

        let app = match properties.max_concurrent_connections {
            Some(max) => router.layer(GlobalConcurrencyLimitLayer::new(max)),
            None => router,
        };
        let make_service = app.into_make_service();

        let handle = Handle::new();
        let drainer = handle.clone();
        let graceful = Duration::from_secs(properties.graceful_timeout);
        tokio::spawn(async move {
            shutdown.await;
            drainer.graceful_shutdown(Some(graceful));
        });

        match &properties.tls {
            Some(tls) => {
                install_crypto_provider();
                let config = RustlsConfig::from_pem_file(&tls.cert_file, &tls.key_file).await?;
                axum_server::from_tcp_rustls(listener, config)
                    .handle(handle)
                    .serve(make_service)
                    .await?;
            }
            None => {
                axum_server::from_tcp(listener)
                    .handle(handle)
                    .serve(make_service)
                    .await?;
            }
        }
        Ok(())
    }
}

/// Binds and serves `router` according to `properties` until `shutdown`
/// resolves — the one-call bootstrap that replaces hand-written
/// `TcpListener::bind` + `axum::serve`. The signature is designed for
/// `firefly_lifecycle::Application::on_server`:
/// `on_server("http", move |sig| serve(router, props, sig.wait()))`.
pub async fn serve<F>(
    router: Router,
    properties: ServerProperties,
    shutdown: F,
) -> Result<(), ServeError>
where
    F: Future<Output = ()> + Send + 'static,
{
    Server::bind(&properties)?.serve(router, shutdown).await
}
