//! The canonical typed error of the framework and its RFC 7807 view.

use std::collections::BTreeMap;
use std::error::Error as StdError;

use serde_json::Value;

use crate::problem::{
    ProblemDetail, TYPE_BAD_REQUEST, TYPE_CONFLICT, TYPE_FORBIDDEN, TYPE_IDEMPOTENCY,
    TYPE_INTERNAL, TYPE_NOT_FOUND, TYPE_RATE_LIMITED, TYPE_UNAUTHORIZED, TYPE_VALIDATION,
};

/// The canonical success-or-failure type of the framework.
///
/// Where the Go port exposes a generic `Result[T]` envelope (because Go
/// lacks a native result type), Rust already has one: this alias pairs
/// the standard [`Result`] with [`FireflyError`], so `map`, `and_then`,
/// and the `?` operator replace Go's `MapResult` / `FlatMapResult` /
/// `Value()` helpers.
pub type FireflyResult<T> = Result<T, FireflyError>;

/// The canonical typed error of the framework. Mirrors the Java
/// `FireflyException`, the .NET `FireflyException`, and the Go
/// `FireflyError`: every error carries a stable code (also used as the
/// RFC 7807 type URI), a human title, an HTTP status, an optional
/// detail, and structured fields.
///
/// Errors wrap a cause via the standard [`std::error::Error::source`]
/// chain.
#[derive(Debug, thiserror::Error)]
#[error("{}: {}", .code, if .detail.is_empty() { .title } else { .detail })]
pub struct FireflyError {
    /// Stable machine-readable code; doubles as the RFC 7807 type URI.
    pub code: String,
    /// Short, human-readable summary.
    pub title: String,
    /// HTTP status code this error maps to.
    pub status: u16,
    /// Explanation specific to this occurrence; may be empty.
    pub detail: String,
    /// Structured extension fields carried onto the RFC 7807 envelope.
    pub fields: BTreeMap<String, Value>,
    /// Underlying cause, exposed through [`std::error::Error::source`].
    #[source]
    pub cause: Option<Box<dyn StdError + Send + Sync + 'static>>,
}

impl FireflyError {
    /// Builds a `FireflyError` with the given code, title, status, and detail.
    pub fn new(
        code: impl Into<String>,
        title: impl Into<String>,
        status: u16,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            title: title.into(),
            status,
            detail: detail.into(),
            fields: BTreeMap::new(),
            cause: None,
        }
    }

    /// Sets an extension field and returns the error.
    #[must_use]
    pub fn with_field(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.fields.insert(key.into(), value.into());
        self
    }

    /// Attaches an underlying cause and returns the error.
    #[must_use]
    pub fn with_cause(mut self, cause: impl StdError + Send + Sync + 'static) -> Self {
        self.cause = Some(Box::new(cause));
        self
    }

    /// Renders the error as an RFC 7807 [`ProblemDetail`].
    pub fn to_problem(&self) -> ProblemDetail {
        let mut pd = ProblemDetail::new(
            self.code.clone(),
            self.title.clone(),
            self.status,
            self.detail.clone(),
        );
        for (k, v) in &self.fields {
            pd = pd.with(k.clone(), v.clone());
        }
        pd
    }

    /// Returns a 400 `FireflyError`.
    pub fn bad_request(detail: impl Into<String>) -> Self {
        Self::new(TYPE_BAD_REQUEST, "Bad Request", 400, detail)
    }

    /// Returns a 401 `FireflyError`.
    pub fn unauthorized(detail: impl Into<String>) -> Self {
        Self::new(TYPE_UNAUTHORIZED, "Unauthorized", 401, detail)
    }

    /// Returns a 403 `FireflyError`.
    pub fn forbidden(detail: impl Into<String>) -> Self {
        Self::new(TYPE_FORBIDDEN, "Forbidden", 403, detail)
    }

    /// Returns a 404 `FireflyError`.
    pub fn not_found(detail: impl Into<String>) -> Self {
        Self::new(TYPE_NOT_FOUND, "Not Found", 404, detail)
    }

    /// Returns a 409 `FireflyError`.
    pub fn conflict(detail: impl Into<String>) -> Self {
        Self::new(TYPE_CONFLICT, "Conflict", 409, detail)
    }

    /// Returns a 422 `FireflyError` tagged for validation failures.
    pub fn validation(detail: impl Into<String>) -> Self {
        Self::new(TYPE_VALIDATION, "Validation Failed", 422, detail)
    }

    /// Returns a 429 `FireflyError`.
    pub fn rate_limited(detail: impl Into<String>) -> Self {
        Self::new(TYPE_RATE_LIMITED, "Too Many Requests", 429, detail)
    }

    /// Returns a 500 `FireflyError`.
    pub fn internal(detail: impl Into<String>) -> Self {
        Self::new(TYPE_INTERNAL, "Internal Server Error", 500, detail)
    }

    /// Returns a 409 `FireflyError` tagged for idempotency conflicts.
    pub fn idempotency_conflict(detail: impl Into<String>) -> Self {
        Self::new(TYPE_IDEMPOTENCY, "Idempotency Conflict", 409, detail)
    }

    /// Returns a 422 `FireflyError` for a violated business invariant —
    /// the Rust analog of pyfly's `BusinessRuleViolation(rule)`.
    ///
    /// The code is `DOMAIN_RULE_VIOLATION` and the rule name is carried
    /// in the structured `rule` field. When `detail` is empty, pyfly's
    /// default message `Business rule violated: <rule>` is used.
    ///
    /// Use this for rules *intrinsic* to the domain (e.g. "an order
    /// cannot be cancelled after it has shipped"); for malformed input
    /// use [`FireflyError::validation`].
    pub fn business_rule(rule: impl Into<String>, detail: impl Into<String>) -> Self {
        let rule = rule.into();
        let detail = detail.into();
        let detail = if detail.is_empty() {
            format!("Business rule violated: {rule}")
        } else {
            detail
        };
        Self::new(
            "DOMAIN_RULE_VIOLATION",
            "Business Rule Violation",
            422,
            detail,
        )
        .with_field("rule", rule)
    }

    /// Returns a 404 `FireflyError` for a repository asked for an
    /// aggregate that does not exist — the Rust analog of pyfly's
    /// `AggregateNotFound(aggregate_type, id)`.
    ///
    /// The code is `DOMAIN_AGGREGATE_NOT_FOUND`; the aggregate type and
    /// id are carried in the structured `aggregate_type` / `id` fields
    /// and in the detail (`<type> with id=<id> not found`).
    pub fn aggregate_not_found(
        aggregate_type: impl Into<String>,
        id: impl std::fmt::Display,
    ) -> Self {
        let aggregate_type = aggregate_type.into();
        let id = id.to_string();
        Self::new(
            "DOMAIN_AGGREGATE_NOT_FOUND",
            "Aggregate Not Found",
            404,
            format!("{aggregate_type} with id={id} not found"),
        )
        .with_field("aggregate_type", aggregate_type)
        .with_field("id", id)
    }
}

/// Walks the [`std::error::Error::source`] chain looking for a
/// [`FireflyError`] — the Rust analog of Go's `errors.As`.
fn find_firefly<'a>(err: &'a (dyn StdError + 'static)) -> Option<&'a FireflyError> {
    let mut current: Option<&(dyn StdError + 'static)> = Some(err);
    while let Some(e) = current {
        if let Some(fe) = e.downcast_ref::<FireflyError>() {
            return Some(fe);
        }
        current = e.source();
    }
    None
}

/// Reports whether `err` is (or wraps, anywhere in its source chain) a
/// [`FireflyError`].
pub fn is_firefly(err: &(dyn StdError + 'static)) -> bool {
    find_firefly(err).is_some()
}

/// Returns the HTTP status code of the underlying [`FireflyError`], or
/// `500` for any other error type.
pub fn status_of(err: &(dyn StdError + 'static)) -> u16 {
    find_firefly(err).map_or(500, |fe| fe.status)
}

/// Returns the [`ProblemDetail`] view of any error: a [`FireflyError`]
/// renders directly; any other error becomes a generic 500 Internal.
pub fn as_problem(err: &(dyn StdError + 'static)) -> ProblemDetail {
    match find_firefly(err) {
        Some(fe) => fe.to_problem(),
        None => ProblemDetail::internal(err.to_string()),
    }
}
