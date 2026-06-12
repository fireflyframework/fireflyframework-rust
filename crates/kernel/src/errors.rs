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
    ///
    /// The detail mirrors pyfly, which builds the message with the
    /// `{id!r}` (Python `repr`) conversion: string ids are wrapped in
    /// single quotes (`'o-1'`) while numeric ids are left bare (`42`).
    /// The structured `id` field carries the unquoted `str(id)` form in
    /// both ports. See [`AggregateId`].
    pub fn aggregate_not_found(aggregate_type: impl Into<String>, id: impl AggregateId) -> Self {
        let aggregate_type = aggregate_type.into();
        let detail = format!(
            "{aggregate_type} with id={} not found",
            id.aggregate_id_repr()
        );
        Self::new(
            "DOMAIN_AGGREGATE_NOT_FOUND",
            "Aggregate Not Found",
            404,
            detail,
        )
        .with_field("aggregate_type", aggregate_type)
        .with_field("id", id.aggregate_id_str())
    }
}

/// The id of an aggregate, formatted for [`FireflyError::aggregate_not_found`]
/// with pyfly parity.
///
/// pyfly's `AggregateNotFound` builds its message with Python's `{id!r}`
/// (`repr`) conversion and stores `str(id)` in the structured context.
/// This trait reproduces both: [`aggregate_id_repr`](AggregateId::aggregate_id_repr)
/// returns the Python-`repr` form (string ids wrapped in single quotes,
/// numeric ids bare) for the human-readable detail, while
/// [`aggregate_id_str`](AggregateId::aggregate_id_str) returns the bare
/// `str(id)` form for the structured `id` field.
pub trait AggregateId {
    /// The Python-`repr`-equivalent rendering used in the error detail.
    fn aggregate_id_repr(&self) -> String;

    /// The bare `str(id)` rendering used in the structured `id` field.
    fn aggregate_id_str(&self) -> String;
}

/// Renders `s` the way Python's `repr(str)` does, for pyfly parity.
///
/// Python wraps the string in single quotes by default. If the string
/// contains a single quote but no double quote, it switches to a
/// double-quote delimiter (so the inner quote need not be escaped);
/// otherwise it uses single quotes and escapes any embedded single
/// quote. Backslashes are always doubled.
fn python_str_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// String-like ids are quoted in the detail, matching Python's
/// `repr('o-1') == "'o-1'"`.
macro_rules! impl_aggregate_id_quoted {
    ($($ty:ty),* $(,)?) => {
        $(
            impl AggregateId for $ty {
                fn aggregate_id_repr(&self) -> String {
                    python_str_repr(self)
                }
                fn aggregate_id_str(&self) -> String {
                    self.to_string()
                }
            }
        )*
    };
}

impl_aggregate_id_quoted!(str, String, &str);

impl AggregateId for &String {
    fn aggregate_id_repr(&self) -> String {
        (**self).aggregate_id_repr()
    }
    fn aggregate_id_str(&self) -> String {
        (**self).aggregate_id_str()
    }
}

/// Numeric ids are left bare in the detail, matching Python's
/// `repr(42) == "42"`.
macro_rules! impl_aggregate_id_bare {
    ($($ty:ty),* $(,)?) => {
        $(
            impl AggregateId for $ty {
                fn aggregate_id_repr(&self) -> String {
                    self.to_string()
                }
                fn aggregate_id_str(&self) -> String {
                    self.to_string()
                }
            }
        )*
    };
}

impl_aggregate_id_bare!(
    u8,
    u16,
    u32,
    u64,
    u128,
    usize,
    i8,
    i16,
    i32,
    i64,
    i128,
    isize,
    uuid::Uuid
);

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
