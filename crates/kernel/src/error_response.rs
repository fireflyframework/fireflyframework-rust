//! Typed, RFC 7807-inspired structured error model — the Rust port of
//! pyfly's `kernel/types.py` (`ErrorCategory`, `ErrorSeverity`,
//! `FieldError`, `ErrorResponse`).
//!
//! This is **additive** over [`ProblemDetail`](crate::ProblemDetail): it
//! does not touch the Go-parity `application/problem+json` wire bytes.
//! Where [`ProblemDetail`] carries the bare RFC 7807 members plus a flat
//! extension map, [`ErrorResponse`] adds first-class **classification**
//! ([`ErrorCategory`] / [`ErrorSeverity`]), **resilience** hints
//! (`retryable` / `retry_after`), **tracing** ids, and per-field
//! validation errors ([`FieldError`]).
//!
//! Its [`to_value`](ErrorResponse::to_value) /
//! [`Serialize`](serde::Serialize) shape matches pyfly's
//! `ErrorResponse.to_dict()` exactly: the core members plus `category`,
//! `severity` and `retryable` are always present, every other optional
//! member is omitted when unset (`None`) or empty, and field names use
//! pyfly's `snake_case` keys (`trace_id`, `field_errors`, `retry_after`,
//! `debug_info`, …) — **not** the `ProblemDetail` wire keys. Pick the
//! model whose wire contract you need: cross-runtime `problem+json`
//! clients consume [`ProblemDetail`]; pyfly-shaped structured-error
//! consumers consume [`ErrorResponse`].

use std::collections::BTreeMap;

use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Classifies an error by its origin or domain. Port of pyfly's
/// `ErrorCategory`; the wire value is the uppercase variant name
/// (`"VALIDATION"`, `"BUSINESS"`, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ErrorCategory {
    /// Malformed or semantically invalid input.
    #[serde(rename = "VALIDATION")]
    Validation,
    /// A violated business invariant / domain rule.
    #[serde(rename = "BUSINESS")]
    Business,
    /// An internal / infrastructure fault.
    #[serde(rename = "TECHNICAL")]
    Technical,
    /// An authentication or authorization failure.
    #[serde(rename = "SECURITY")]
    Security,
    /// A failure originating in an upstream / external dependency.
    #[serde(rename = "EXTERNAL")]
    External,
    /// A missing or unavailable resource.
    #[serde(rename = "RESOURCE")]
    Resource,
    /// A rate-limit / throttling rejection.
    #[serde(rename = "RATE_LIMIT")]
    RateLimit,
    /// A circuit-breaker open rejection.
    #[serde(rename = "CIRCUIT_BREAKER")]
    CircuitBreaker,
}

impl ErrorCategory {
    /// Returns the wire string (uppercase variant name), matching
    /// pyfly's `ErrorCategory.value`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCategory::Validation => "VALIDATION",
            ErrorCategory::Business => "BUSINESS",
            ErrorCategory::Technical => "TECHNICAL",
            ErrorCategory::Security => "SECURITY",
            ErrorCategory::External => "EXTERNAL",
            ErrorCategory::Resource => "RESOURCE",
            ErrorCategory::RateLimit => "RATE_LIMIT",
            ErrorCategory::CircuitBreaker => "CIRCUIT_BREAKER",
        }
    }
}

/// pyfly default: [`ErrorCategory::Technical`].
impl Default for ErrorCategory {
    fn default() -> Self {
        ErrorCategory::Technical
    }
}

/// Indicates the severity level of an error. Port of pyfly's
/// `ErrorSeverity`; the wire value is the uppercase variant name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ErrorSeverity {
    /// Informational; safe to ignore in most flows.
    #[serde(rename = "LOW")]
    Low,
    /// The default — a recoverable failure.
    #[serde(rename = "MEDIUM")]
    Medium,
    /// A serious failure that warrants attention.
    #[serde(rename = "HIGH")]
    High,
    /// A critical failure (data loss, outage).
    #[serde(rename = "CRITICAL")]
    Critical,
}

impl ErrorSeverity {
    /// Returns the wire string (uppercase variant name), matching
    /// pyfly's `ErrorSeverity.value`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorSeverity::Low => "LOW",
            ErrorSeverity::Medium => "MEDIUM",
            ErrorSeverity::High => "HIGH",
            ErrorSeverity::Critical => "CRITICAL",
        }
    }
}

/// pyfly default: [`ErrorSeverity::Medium`].
impl Default for ErrorSeverity {
    fn default() -> Self {
        ErrorSeverity::Medium
    }
}

/// Describes a validation error on a single field — port of pyfly's
/// `FieldError`.
///
/// Serializes (and deserializes) with the `field`, `message` and
/// `rejected_value` keys. `rejected_value` is a free-form
/// [`serde_json::Value`]; pyfly's `dataclasses.asdict` always includes
/// it (it is `null` when unset), so this type does the same.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldError {
    /// The name of the offending field.
    pub field: String,
    /// A human-readable description of the violation.
    pub message: String,
    /// The value that was rejected (`null` when not supplied).
    #[serde(default)]
    pub rejected_value: Value,
}

impl FieldError {
    /// Builds a `FieldError` with no rejected value (`null`).
    pub fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
            rejected_value: Value::Null,
        }
    }

    /// Sets the rejected value and returns the field error.
    #[must_use]
    pub fn with_rejected_value(mut self, value: impl Into<Value>) -> Self {
        self.rejected_value = value.into();
        self
    }
}

/// An RFC 7807-inspired **structured** error response — the Rust port of
/// pyfly's `ErrorResponse`.
///
/// Core members are always emitted; optional members are omitted from
/// the serialized form when `None` or empty (see the module docs). Build
/// one with [`ErrorResponse::new`] then chain the `with_*` setters, or
/// construct the struct literally.
///
/// ```
/// use firefly_kernel::{ErrorCategory, ErrorResponse, ErrorSeverity, FieldError};
///
/// let resp = ErrorResponse::new(
///     "2026-06-12T00:00:00Z",
///     400,
///     "Bad Request",
///     "name must not be blank",
///     "VALIDATION_FAILED",
///     "/users",
/// )
/// .with_category(ErrorCategory::Validation)
/// .with_severity(ErrorSeverity::Low)
/// .with_field_error(FieldError::new("name", "must not be blank"));
///
/// let v = resp.to_value();
/// assert_eq!(v["category"], "VALIDATION");
/// assert_eq!(v["retryable"], false); // always present
/// assert_eq!(v["field_errors"][0]["field"], "name");
/// assert!(v.get("trace_id").is_none()); // omitted when unset
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct ErrorResponse {
    // --- Core (always present) ---
    /// ISO-8601 timestamp of the occurrence.
    pub timestamp: String,
    /// HTTP status code.
    pub status: u16,
    /// Short, human-readable error name (e.g. `"Bad Request"`).
    pub error: String,
    /// Explanation specific to this occurrence.
    pub message: String,
    /// Stable machine-readable code.
    pub code: String,
    /// The request path that produced the error.
    pub path: String,

    // --- Tracing (optional) ---
    /// Distributed-trace id, if present.
    pub trace_id: Option<String>,
    /// Span id within the trace, if present.
    pub span_id: Option<String>,
    /// Business transaction id, if present.
    pub transaction_id: Option<String>,

    // --- Classification (always present) ---
    /// The error category (default [`ErrorCategory::Technical`]).
    pub category: ErrorCategory,
    /// The error severity (default [`ErrorSeverity::Medium`]).
    pub severity: ErrorSeverity,

    // --- Resilience ---
    /// Whether the caller may retry (always present, default `false`).
    pub retryable: bool,
    /// Suggested seconds to wait before retrying, if any.
    pub retry_after: Option<i64>,

    // --- Validation ---
    /// Per-field validation errors (omitted when empty).
    pub field_errors: Vec<FieldError>,

    // --- Debug ---
    /// Free-form debug context (omitted when `None`).
    pub debug_info: Option<BTreeMap<String, Value>>,
    /// A remediation suggestion, if any.
    pub suggestion: Option<String>,
    /// A documentation URL, if any.
    pub documentation_url: Option<String>,
}

impl ErrorResponse {
    /// Builds an `ErrorResponse` with the six core members set and every
    /// optional member at its pyfly default (category
    /// [`ErrorCategory::Technical`], severity [`ErrorSeverity::Medium`],
    /// `retryable = false`, all optionals unset).
    pub fn new(
        timestamp: impl Into<String>,
        status: u16,
        error: impl Into<String>,
        message: impl Into<String>,
        code: impl Into<String>,
        path: impl Into<String>,
    ) -> Self {
        Self {
            timestamp: timestamp.into(),
            status,
            error: error.into(),
            message: message.into(),
            code: code.into(),
            path: path.into(),
            trace_id: None,
            span_id: None,
            transaction_id: None,
            category: ErrorCategory::default(),
            severity: ErrorSeverity::default(),
            retryable: false,
            retry_after: None,
            field_errors: Vec::new(),
            debug_info: None,
            suggestion: None,
            documentation_url: None,
        }
    }

    /// Sets the [`ErrorCategory`] and returns the response.
    #[must_use]
    pub fn with_category(mut self, category: ErrorCategory) -> Self {
        self.category = category;
        self
    }

    /// Sets the [`ErrorSeverity`] and returns the response.
    #[must_use]
    pub fn with_severity(mut self, severity: ErrorSeverity) -> Self {
        self.severity = severity;
        self
    }

    /// Marks the error retryable and returns the response.
    #[must_use]
    pub fn with_retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    /// Sets `retry_after` (seconds) and returns the response.
    #[must_use]
    pub fn with_retry_after(mut self, seconds: i64) -> Self {
        self.retry_after = Some(seconds);
        self
    }

    /// Sets the trace id and returns the response.
    #[must_use]
    pub fn with_trace_id(mut self, trace_id: impl Into<String>) -> Self {
        self.trace_id = Some(trace_id.into());
        self
    }

    /// Sets the span id and returns the response.
    #[must_use]
    pub fn with_span_id(mut self, span_id: impl Into<String>) -> Self {
        self.span_id = Some(span_id.into());
        self
    }

    /// Sets the transaction id and returns the response.
    #[must_use]
    pub fn with_transaction_id(mut self, transaction_id: impl Into<String>) -> Self {
        self.transaction_id = Some(transaction_id.into());
        self
    }

    /// Appends a [`FieldError`] and returns the response.
    #[must_use]
    pub fn with_field_error(mut self, error: FieldError) -> Self {
        self.field_errors.push(error);
        self
    }

    /// Replaces the field-error list and returns the response.
    #[must_use]
    pub fn with_field_errors(mut self, errors: Vec<FieldError>) -> Self {
        self.field_errors = errors;
        self
    }

    /// Sets the debug-info map and returns the response.
    #[must_use]
    pub fn with_debug_info(mut self, debug_info: BTreeMap<String, Value>) -> Self {
        self.debug_info = Some(debug_info);
        self
    }

    /// Sets the remediation suggestion and returns the response.
    #[must_use]
    pub fn with_suggestion(mut self, suggestion: impl Into<String>) -> Self {
        self.suggestion = Some(suggestion.into());
        self
    }

    /// Sets the documentation URL and returns the response.
    #[must_use]
    pub fn with_documentation_url(mut self, url: impl Into<String>) -> Self {
        self.documentation_url = Some(url.into());
        self
    }

    /// Serializes to a [`serde_json::Value`] with pyfly's
    /// `to_dict()` shape: core members + `category` + `severity` +
    /// `retryable` always present; optional scalars omitted when `None`;
    /// `field_errors` omitted when empty; `debug_info` omitted when
    /// `None`.
    #[must_use]
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

impl Serialize for ErrorResponse {
    /// Emits pyfly's `ErrorResponse.to_dict()` shape. `serde(skip)`
    /// would drop `category`/`severity`/`retryable` defaults too, so the
    /// omit-when-empty logic is written out explicitly to keep those
    /// three always-present while dropping unset optionals.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        // Always-present core members.
        map.serialize_entry("timestamp", &self.timestamp)?;
        map.serialize_entry("status", &self.status)?;
        map.serialize_entry("error", &self.error)?;
        map.serialize_entry("message", &self.message)?;
        map.serialize_entry("code", &self.code)?;
        map.serialize_entry("path", &self.path)?;
        map.serialize_entry("category", &self.category)?;
        map.serialize_entry("severity", &self.severity)?;
        map.serialize_entry("retryable", &self.retryable)?;

        // Optional scalars — included only when present.
        if let Some(trace_id) = &self.trace_id {
            map.serialize_entry("trace_id", trace_id)?;
        }
        if let Some(span_id) = &self.span_id {
            map.serialize_entry("span_id", span_id)?;
        }
        if let Some(transaction_id) = &self.transaction_id {
            map.serialize_entry("transaction_id", transaction_id)?;
        }
        if let Some(retry_after) = self.retry_after {
            map.serialize_entry("retry_after", &retry_after)?;
        }
        if let Some(suggestion) = &self.suggestion {
            map.serialize_entry("suggestion", suggestion)?;
        }
        if let Some(documentation_url) = &self.documentation_url {
            map.serialize_entry("documentation_url", documentation_url)?;
        }

        // Field errors — included only when non-empty.
        if !self.field_errors.is_empty() {
            map.serialize_entry("field_errors", &self.field_errors)?;
        }

        // Debug info — included only when present.
        if let Some(debug_info) = &self.debug_info {
            map.serialize_entry("debug_info", debug_info)?;
        }

        map.end()
    }
}
