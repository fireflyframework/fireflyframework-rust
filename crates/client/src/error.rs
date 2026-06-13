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

    /// A GraphQL response carried a non-empty `errors` array. The raw
    /// error objects are preserved verbatim (each is the spec's
    /// `{ message, locations?, path?, extensions? }` shape) so callers
    /// can inspect `message`, `path`, and `extensions` — the Rust analog
    /// of pyfly's `RuntimeError(f"GraphQL errors: {data['errors']}")`,
    /// except the structured array is kept instead of stringified.
    #[error("client: graphql errors: {0:?}")]
    GraphQl(Vec<serde_json::Value>),

    /// The SOAP / gRPC / WebSocket placeholder sentinel — mirrors Go's
    /// `ErrTransportNotRegistered`. Returned by [`new_soap`](crate::new_soap)
    /// / [`new_grpc`](crate::new_grpc) / [`new_websocket`](crate::new_websocket)
    /// and by the feature-gated builders when their feature is disabled.
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

    /// Reports whether the upstream status equals `status`.
    fn status_is(&self, status: u16) -> bool {
        self.status() == Some(status)
    }

    /// Reports whether this is an HTTP **400 Bad Request** — the analog of
    /// pyfly's `ServiceValidationException`.
    pub fn is_validation(&self) -> bool {
        self.status_is(400)
    }

    /// Reports whether this is an HTTP **401 Unauthorized** or **403
    /// Forbidden** — the analog of pyfly's `ServiceAuthenticationException`.
    pub fn is_unauthorized(&self) -> bool {
        self.status_is(401) || self.status_is(403)
    }

    /// Reports whether this is an HTTP **404 Not Found** — the analog of
    /// pyfly's `ServiceNotFoundException`.
    pub fn is_not_found(&self) -> bool {
        self.status_is(404)
    }

    /// Reports whether this is an HTTP **409 Conflict** — the analog of
    /// pyfly's `ServiceConflictException`.
    pub fn is_conflict(&self) -> bool {
        self.status_is(409)
    }

    /// Reports whether this is an HTTP **422 Unprocessable Entity** — the
    /// analog of pyfly's `ServiceUnprocessableEntityException`.
    pub fn is_unprocessable_entity(&self) -> bool {
        self.status_is(422)
    }

    /// Reports whether this is an HTTP **429 Too Many Requests** — the
    /// analog of pyfly's `ServiceRateLimitException` (which is `retryable`).
    pub fn is_rate_limited(&self) -> bool {
        self.status_is(429)
    }

    /// Reports whether this is an upstream **5xx** server error — the analog
    /// of pyfly's `ServiceUnavailableException` (which is `retryable`).
    pub fn is_server_error(&self) -> bool {
        self.status().is_some_and(|s| (500..600).contains(&s))
    }

    #[cfg(test)]
    fn problem(status: u16) -> Self {
        Self::Problem(FireflyError::new("CODE", "Title", status, "detail"))
    }

    /// Reports whether the failure is worth retrying — the same rule
    /// [`RestClient`](crate::RestClient) applies internally and the analog of
    /// pyfly's `ServiceClientException.retryable` flag.
    ///
    /// Returns `true` for transport failures (no response was obtained) and
    /// for upstream errors whose status is `429` (rate limited) or any `5xx`
    /// (server error). Decode / encode / url / GraphQL /
    /// `TransportNotRegistered` / exhausted errors are never retryable.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Transport(_) => true,
            Self::Problem(fe) => fe.status == 429 || (500..600).contains(&fe.status),
            Self::GraphQl(_)
            | Self::InvalidUrl(_)
            | Self::Encode(_)
            | Self::Decode(_)
            | Self::Exhausted(_)
            | Self::TransportNotRegistered => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_predicates_classify_problems_like_pyfly_exceptions() {
        assert!(ClientError::problem(400).is_validation());
        assert!(ClientError::problem(401).is_unauthorized());
        assert!(ClientError::problem(403).is_unauthorized());
        assert!(ClientError::problem(404).is_not_found());
        assert!(ClientError::problem(409).is_conflict());
        assert!(ClientError::problem(422).is_unprocessable_entity());
        assert!(ClientError::problem(429).is_rate_limited());
        assert!(ClientError::problem(500).is_server_error());
        assert!(ClientError::problem(503).is_server_error());

        // Negative cases: a 404 is none of the others.
        let nf = ClientError::problem(404);
        assert!(!nf.is_validation());
        assert!(!nf.is_unauthorized());
        assert!(!nf.is_conflict());
        assert!(!nf.is_rate_limited());
        assert!(!nf.is_server_error());
        assert_eq!(nf.status(), Some(404));
    }

    #[test]
    fn is_retryable_matches_the_rest_client_rule() {
        // 429 and every 5xx are retryable.
        assert!(ClientError::problem(429).is_retryable());
        assert!(ClientError::problem(500).is_retryable());
        assert!(ClientError::problem(599).is_retryable());
        // Permanent 4xx are not.
        assert!(!ClientError::problem(400).is_retryable());
        assert!(!ClientError::problem(404).is_retryable());
        // Non-HTTP error classes are never retryable (except transport).
        assert!(!ClientError::InvalidUrl("x".into()).is_retryable());
        assert!(!ClientError::Exhausted(3).is_retryable());
        assert!(!ClientError::TransportNotRegistered.is_retryable());
        assert!(!ClientError::GraphQl(vec![]).is_retryable());
    }

    #[test]
    fn non_problem_errors_have_no_status() {
        assert_eq!(ClientError::InvalidUrl("x".into()).status(), None);
        assert!(!ClientError::InvalidUrl("x".into()).is_not_found());
    }
}
