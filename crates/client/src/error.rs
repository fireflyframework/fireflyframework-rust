//! The typed error family of the outbound client.

use firefly_kernel::FireflyError;

/// Errors produced by [`RestClient`](crate::RestClient) and the
/// SOAP / gRPC / WebSocket placeholder builders.
///
/// The Go port returns a plain `error` from `Do` and lets callers fish
/// the `*kernel.FireflyError` out with `errors.As`; the Rust port makes
/// the possible failure classes explicit in one `thiserror` enum. Use
/// [`ClientError::as_firefly`] (the `errors.As` analog) or pattern-match
/// on [`ClientError::Problem`] to reach the decoded upstream error.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// A non-2xx upstream response, decoded into the kernel's canonical
    /// [`FireflyError`]. When the response carried an RFC 7807
    /// `application/problem+json` body, `code`, `title`, `status`,
    /// `detail`, and `fields` are populated from it; otherwise the
    /// error carries the HTTP status, its canonical reason phrase as
    /// the title, and the raw response body as the detail.
    #[error(transparent)]
    Problem(#[from] FireflyError),

    /// A network / transport failure reported by the underlying
    /// `reqwest` client (connect refused, timeout, TLS, …). Transport
    /// failures are retried up to the configured attempt budget.
    #[error("client: transport error: {0}")]
    Transport(#[from] reqwest::Error),

    /// The base URL + path did not parse as a valid URL. Returned
    /// before any request is sent and never retried — the analog of
    /// Go's `http.NewRequestWithContext` error path.
    #[error("client: invalid url: {0}")]
    InvalidUrl(String),

    /// The request body failed to JSON-encode. Returned before any
    /// request is sent and never retried.
    #[error("client: encode error: {0}")]
    Encode(#[source] serde_json::Error),

    /// A 2xx response body failed to JSON-decode into the requested
    /// output type.
    #[error("client: decode error: {0}")]
    Decode(#[source] serde_json::Error),

    /// Every configured attempt was consumed without obtaining a final
    /// response and no more specific error was recorded — mirrors Go's
    /// `client: exhausted %d attempts` sentinel (reached when the
    /// attempt budget is zero).
    #[error("client: exhausted {0} attempts")]
    Exhausted(usize),

    /// The SOAP / gRPC / WebSocket placeholder sentinel — mirrors Go's
    /// `ErrTransportNotRegistered`. Production adapters live in
    /// dedicated transport modules.
    #[error("firefly/client: transport adapter not registered")]
    TransportNotRegistered,
}

impl ClientError {
    /// Returns the decoded upstream [`FireflyError`] when this error is
    /// a [`ClientError::Problem`] — the Rust analog of Go's
    /// `errors.As(err, &fe)`.
    pub fn as_firefly(&self) -> Option<&FireflyError> {
        match self {
            Self::Problem(fe) => Some(fe),
            _ => None,
        }
    }

    /// Returns the upstream HTTP status when this error carries one.
    pub fn status(&self) -> Option<u16> {
        self.as_firefly().map(|fe| fe.status)
    }
}
