//! Authorization decision types and the bus middleware enforcing them.
//!
//! The Rust port of pyfly's `pyfly.cqrs.authorization` package (Java's
//! `AuthorizationService` from `firefly-common-cqrs`). The message's own
//! [`Message::authorize`](crate::Message::authorize) hook produces an
//! [`AuthorizationResult`]; [`AuthorizationMiddleware`] runs the hook on
//! every dispatch and converts a denial into
//! [`CqrsError::Authorization`](crate::CqrsError::Authorization) —
//! pyfly's `AuthorizationException`.

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::bus::{DynHandler, Envelope, HandlerFuture, Middleware};
use crate::CqrsError;

/// Default error code stamped on an [`AuthorizationError`] — pyfly's
/// `error_code="AUTHORIZATION_ERROR"`.
pub const AUTHORIZATION_ERROR_CODE: &str = "AUTHORIZATION_ERROR";

/// Severity level for an authorization error — pyfly's
/// `AuthorizationSeverity` StrEnum. Serializes to the same wire strings
/// (`"WARNING"` / `"ERROR"` / `"CRITICAL"`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AuthorizationSeverity {
    /// Logged but advisory.
    Warning,
    /// Standard denial (the default).
    #[default]
    Error,
    /// Severe denial worth alerting on.
    Critical,
}

impl AuthorizationSeverity {
    /// The wire string for the severity — pyfly's StrEnum `.value`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Warning => "WARNING",
            Self::Error => "ERROR",
            Self::Critical => "CRITICAL",
        }
    }
}

impl fmt::Display for AuthorizationSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A single authorization failure — pyfly's frozen `AuthorizationError`
/// dataclass.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorizationError {
    /// Resource the denial applies to, e.g. `"orders"`.
    pub resource: String,
    /// Human-readable denial message.
    pub message: String,
    /// Machine-readable error code; defaults to
    /// [`AUTHORIZATION_ERROR_CODE`].
    pub error_code: String,
    /// Severity; defaults to [`AuthorizationSeverity::Error`].
    pub severity: AuthorizationSeverity,
    /// The denied action (e.g. `"DELETE"`), when known.
    pub denied_action: Option<String>,
}

impl AuthorizationError {
    /// Builds an error with the pyfly defaults: code
    /// [`AUTHORIZATION_ERROR_CODE`], severity
    /// [`AuthorizationSeverity::Error`], no denied action.
    pub fn new(resource: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            resource: resource.into(),
            message: message.into(),
            error_code: AUTHORIZATION_ERROR_CODE.to_string(),
            severity: AuthorizationSeverity::Error,
            denied_action: None,
        }
    }

    /// Replaces the error code — pyfly's `error_code=` keyword.
    #[must_use]
    pub fn with_error_code(mut self, error_code: impl Into<String>) -> Self {
        self.error_code = error_code.into();
        self
    }

    /// Replaces the severity — pyfly's `severity=` keyword.
    #[must_use]
    pub fn with_severity(mut self, severity: AuthorizationSeverity) -> Self {
        self.severity = severity;
        self
    }

    /// Sets the denied action — pyfly's `denied_action=` keyword.
    #[must_use]
    pub fn with_denied_action(mut self, denied_action: impl Into<String>) -> Self {
        self.denied_action = Some(denied_action.into());
        self
    }
}

/// Immutable authorization decision — pyfly's `AuthorizationResult`
/// frozen dataclass. Compose multiple results with
/// [`AuthorizationResult::combine`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AuthorizationResult {
    authorized: bool,
    errors: Vec<AuthorizationError>,
    summary: Option<String>,
}

impl AuthorizationResult {
    /// An authorized decision with no errors — pyfly's
    /// `AuthorizationResult.success()`.
    pub fn success() -> Self {
        Self {
            authorized: true,
            errors: Vec::new(),
            summary: None,
        }
    }

    /// A denial carrying a single error with default code and severity —
    /// pyfly's `AuthorizationResult.failure(resource, message)`. Use
    /// [`AuthorizationResult::failure_with`] to customise the error.
    pub fn failure(resource: impl Into<String>, message: impl Into<String>) -> Self {
        Self::failure_with(AuthorizationError::new(resource, message))
    }

    /// A denial carrying the given fully-specified error — covers
    /// pyfly's `failure(..., error_code=..., denied_action=...)`
    /// keywords.
    pub fn failure_with(error: AuthorizationError) -> Self {
        Self {
            authorized: false,
            errors: vec![error],
            summary: None,
        }
    }

    /// Whether the decision authorizes the dispatch — pyfly's
    /// `.authorized` field.
    pub fn is_authorized(&self) -> bool {
        self.authorized
    }

    /// The accumulated errors (empty on success).
    pub fn errors(&self) -> &[AuthorizationError] {
        &self.errors
    }

    /// The optional human-readable summary.
    pub fn summary(&self) -> Option<&str> {
        self.summary.as_deref()
    }

    /// Replaces the summary used by the [`fmt::Display`] rendering.
    #[must_use]
    pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = Some(summary.into());
        self
    }

    /// Merges two results: unauthorized if either is unauthorized,
    /// errors concatenated — pyfly's `combine` (the summary is reset,
    /// exactly like pyfly's).
    #[must_use]
    pub fn combine(mut self, other: AuthorizationResult) -> Self {
        self.errors.extend(other.errors);
        Self {
            authorized: self.authorized && other.authorized,
            errors: self.errors,
            summary: None,
        }
    }

    /// Renders each error as `"<resource>: <message>"` — pyfly's
    /// `error_messages()`.
    pub fn error_messages(&self) -> Vec<String> {
        self.errors
            .iter()
            .map(|e| format!("{}: {}", e.resource, e.message))
            .collect()
    }
}

impl fmt::Display for AuthorizationResult {
    /// The denial summary used by
    /// [`CqrsError::Authorization`](crate::CqrsError::Authorization):
    /// the explicit summary if set, else the joined
    /// [`AuthorizationResult::error_messages`], else
    /// `"Authorization denied"` — exactly pyfly's
    /// `AuthorizationException` message derivation. An authorized
    /// result renders as `"authorized"`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.authorized {
            return f.write_str("authorized");
        }
        if let Some(summary) = &self.summary {
            return f.write_str(summary);
        }
        let messages = self.error_messages();
        if messages.is_empty() {
            f.write_str("Authorization denied")
        } else {
            f.write_str(&messages.join("; "))
        }
    }
}

/// Middleware that runs the message's
/// [`Message::authorize`](crate::Message::authorize) hook before
/// dispatch and short-circuits with
/// [`CqrsError::Authorization`](crate::CqrsError::Authorization) on
/// denial.
///
/// The Rust spelling of pyfly's `AuthorizationService` wired into the
/// command/query buses: when constructed [`AuthorizationMiddleware::disabled`]
/// (pyfly's `enabled=False`) every dispatch is automatically authorized.
/// The hook receives the dispatch's [`ExecutionContext`](crate::ExecutionContext)
/// (pyfly's `authorize_with_context`) when one was attached via
/// [`Bus::send_with_context`](crate::Bus::send_with_context) or a fluent
/// builder, and `None` otherwise (pyfly's plain `authorize()`).
///
/// Messages that keep the default (always-authorized) hook pass through
/// untouched — the same pattern as
/// [`ValidationMiddleware`](crate::ValidationMiddleware).
#[derive(Clone, Copy, Debug)]
pub struct AuthorizationMiddleware {
    enabled: bool,
}

impl Default for AuthorizationMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthorizationMiddleware {
    /// Returns the middleware with authorization enabled — pyfly's
    /// `AuthorizationService()` default.
    pub fn new() -> Self {
        Self { enabled: true }
    }

    /// Returns a disabled middleware that authorizes everything —
    /// pyfly's `AuthorizationService(enabled=False)`.
    pub fn disabled() -> Self {
        Self { enabled: false }
    }

    /// Returns the middleware with the given enablement — pyfly's
    /// `enabled=` keyword.
    pub fn with_enabled(enabled: bool) -> Self {
        Self { enabled }
    }

    /// Whether authorization is enforced — pyfly's `is_enabled`.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
}

impl Middleware for AuthorizationMiddleware {
    fn wrap(&self, next: DynHandler) -> DynHandler {
        let enabled = self.enabled;
        Arc::new(move |env: Arc<Envelope>| -> HandlerFuture {
            let next = Arc::clone(&next);
            Box::pin(async move {
                if enabled {
                    let result = env.authorize();
                    if !result.is_authorized() {
                        return Err(CqrsError::Authorization(result));
                    }
                }
                next(env).await
            })
        })
    }
}
