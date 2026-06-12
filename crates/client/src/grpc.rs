//! gRPC channel builder over [`tonic`] — feature `grpc`.
//!
//! The Rust port of pyfly's `GrpcClientBuilder`: a thin factory that
//! builds a [`tonic::transport::Channel`] for a caller-supplied,
//! code-generated stub. Like pyfly, it deliberately does **not** depend
//! on any specific protobuf-generated client — you pass the channel to
//! your own `FooServiceClient::new(channel)`.
//!
//! pyfly distinguishes `secure_channel` (TLS via
//! `ssl_channel_credentials`) from `insecure_channel`; the Rust port
//! mirrors that with [`GrpcBuilder::secured`]. TLS support requires
//! tonic's `tls` feature on the consuming application — when it is not
//! enabled, [`GrpcBuilder::connect`] on a secured target returns
//! [`GrpcError::TlsUnsupported`] rather than silently downgrading.

use std::time::Duration;

use tonic::transport::{Channel, Endpoint};

/// Errors from [`GrpcBuilder::connect`].
#[derive(Debug, thiserror::Error)]
pub enum GrpcError {
    /// The target was empty — pyfly raises `ValueError("requires a target")`.
    #[error("grpc: builder requires a target")]
    MissingTarget,

    /// The target did not parse as a valid endpoint URI.
    #[error("grpc: invalid target: {0}")]
    InvalidTarget(String),

    /// A secured (TLS) channel was requested but this build of `tonic`
    /// has no TLS support. Enable tonic's `tls` feature in the consuming
    /// application to use [`GrpcBuilder::secured`].
    #[error("grpc: TLS requested but tonic was built without the `tls` feature")]
    TlsUnsupported,

    /// The transport failed to connect.
    #[error("grpc: transport error: {0}")]
    Transport(#[from] tonic::transport::Error),
}

/// Fluently builds a [`tonic::transport::Channel`] — the Rust analog of
/// pyfly's `GrpcClientBuilder`.
///
/// ```
/// use firefly_client::GrpcBuilder;
///
/// let builder = GrpcBuilder::new("http://127.0.0.1:50051")
///     .secured(false)
///     .with_connect_timeout(std::time::Duration::from_secs(5));
/// # let _ = builder;
/// ```
#[derive(Debug, Clone)]
pub struct GrpcBuilder {
    target: String,
    secure: bool,
    connect_timeout: Option<Duration>,
    timeout: Option<Duration>,
}

impl GrpcBuilder {
    /// Returns a builder primed for the given target. tonic expects a
    /// URI (`http://host:port` / `https://host:port`); a bare
    /// `host:port` is accepted and defaulted to `http://`.
    pub fn new(target: impl AsRef<str>) -> Self {
        Self {
            target: target.as_ref().to_owned(),
            secure: false,
            connect_timeout: None,
            timeout: None,
        }
    }

    /// Sets the target (pyfly's `with_target`).
    #[must_use]
    pub fn with_target(mut self, target: impl AsRef<str>) -> Self {
        self.target = target.as_ref().to_owned();
        self
    }

    /// Selects a secure (TLS) channel when `value` is `true`, otherwise
    /// an insecure one — pyfly's `secured(value=True)`.
    #[must_use]
    pub fn secured(mut self, value: bool) -> Self {
        self.secure = value;
        self
    }

    /// Sets the connection-establishment timeout.
    #[must_use]
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = Some(timeout);
        self
    }

    /// Sets the per-request timeout applied to every RPC on the channel.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Whether a secure (TLS) channel was requested.
    #[must_use]
    pub fn is_secured(&self) -> bool {
        self.secure
    }

    /// The configured target.
    #[must_use]
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Builds the [`Endpoint`] without connecting — useful for callers
    /// that want to apply tonic configuration the builder does not
    /// expose (TLS config, concurrency limits, …) before connecting.
    ///
    /// # Errors
    ///
    /// [`GrpcError::MissingTarget`] when the target is empty,
    /// [`GrpcError::InvalidTarget`] when it does not parse, and
    /// [`GrpcError::TlsUnsupported`] when a secured channel is requested
    /// but tonic lacks TLS support.
    pub fn endpoint(&self) -> Result<Endpoint, GrpcError> {
        if self.target.is_empty() {
            return Err(GrpcError::MissingTarget);
        }
        let uri = normalize_target(&self.target);
        let mut endpoint =
            Endpoint::from_shared(uri).map_err(|e| GrpcError::InvalidTarget(e.to_string()))?;
        if self.secure {
            endpoint = apply_tls(endpoint)?;
        }
        if let Some(t) = self.connect_timeout {
            endpoint = endpoint.connect_timeout(t);
        }
        if let Some(t) = self.timeout {
            endpoint = endpoint.timeout(t);
        }
        Ok(endpoint)
    }

    /// Eagerly establishes the channel — the Rust analog of pyfly's
    /// `channel()` followed by a connect. The returned [`Channel`] is
    /// cheap to clone and is handed to a generated stub
    /// (`MyServiceClient::new(channel)`).
    ///
    /// # Errors
    ///
    /// See [`GrpcBuilder::endpoint`] for configuration errors, plus
    /// [`GrpcError::Transport`] when the connection cannot be opened.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # async fn demo() -> Result<(), firefly_client::GrpcError> {
    /// use firefly_client::GrpcBuilder;
    ///
    /// let channel = GrpcBuilder::new("http://127.0.0.1:50051").connect().await?;
    /// // let mut stub = my_proto::greeter_client::GreeterClient::new(channel);
    /// # let _ = channel;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn connect(&self) -> Result<Channel, GrpcError> {
        let endpoint = self.endpoint()?;
        endpoint.connect().await.map_err(GrpcError::Transport)
    }

    /// Builds a lazily-connecting channel — connection is deferred until
    /// the first RPC. Never performs network I/O itself, so it cannot
    /// fail on transport: only the configuration errors of
    /// [`GrpcBuilder::endpoint`] are possible.
    ///
    /// # Errors
    ///
    /// See [`GrpcBuilder::endpoint`].
    pub fn connect_lazy(&self) -> Result<Channel, GrpcError> {
        Ok(self.endpoint()?.connect_lazy())
    }
}

/// Prefixes a bare `host:port` target with `http://` (or `https://` when
/// secured); leaves an explicit scheme untouched.
fn normalize_target(target: &str) -> String {
    if target.contains("://") {
        target.to_owned()
    } else {
        format!("http://{target}")
    }
}

#[cfg(feature = "grpc-tls")]
fn apply_tls(endpoint: Endpoint) -> Result<Endpoint, GrpcError> {
    endpoint
        .tls_config(tonic::transport::ClientTlsConfig::new().with_native_roots())
        .map_err(GrpcError::Transport)
}

#[cfg(not(feature = "grpc-tls"))]
fn apply_tls(_endpoint: Endpoint) -> Result<Endpoint, GrpcError> {
    Err(GrpcError::TlsUnsupported)
}
