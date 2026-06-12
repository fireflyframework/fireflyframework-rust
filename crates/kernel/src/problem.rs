//! RFC 7807 `application/problem+json` envelope.

use std::collections::BTreeMap;

use serde::de::Deserializer;
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The IANA media type for `application/problem+json`.
pub const PROBLEM_CONTENT_TYPE: &str = "application/problem+json";

// Standard problem type URIs — match the strings emitted by the Java,
// .NET, Go, and Python ports so cross-runtime clients dispatch on
// identical values.

/// Type URI for 400 Bad Request problems.
pub const TYPE_BAD_REQUEST: &str = "https://fireflyframework.org/problems/bad-request";
/// Type URI for 401 Unauthorized problems.
pub const TYPE_UNAUTHORIZED: &str = "https://fireflyframework.org/problems/unauthorized";
/// Type URI for 403 Forbidden problems.
pub const TYPE_FORBIDDEN: &str = "https://fireflyframework.org/problems/forbidden";
/// Type URI for 404 Not Found problems.
pub const TYPE_NOT_FOUND: &str = "https://fireflyframework.org/problems/not-found";
/// Type URI for 409 Conflict problems.
pub const TYPE_CONFLICT: &str = "https://fireflyframework.org/problems/conflict";
/// Type URI for 422 Unprocessable Entity problems.
pub const TYPE_UNPROCESSABLE: &str = "https://fireflyframework.org/problems/unprocessable-entity";
/// Type URI for 429 Too Many Requests problems.
pub const TYPE_RATE_LIMITED: &str = "https://fireflyframework.org/problems/rate-limited";
/// Type URI for 500 Internal Server Error problems.
pub const TYPE_INTERNAL: &str = "https://fireflyframework.org/problems/internal-error";
/// Type URI for semantic validation failures (422).
pub const TYPE_VALIDATION: &str = "https://fireflyframework.org/problems/validation";
/// Type URI for idempotency conflicts (409).
pub const TYPE_IDEMPOTENCY: &str = "https://fireflyframework.org/problems/idempotency-conflict";

/// An RFC 7807 `application/problem+json` object.
///
/// Wire-compatible with the Java firefly-common `ErrorEnvelope`, the
/// .NET `FireflyFramework.Kernel` `ProblemDetail`, and the Go kernel
/// `ProblemDetail` — the same field names, same JSON shape, same
/// default type URIs. Empty standard members are omitted on the wire,
/// and [extension members](ProblemDetail::extensions) are flattened
/// into the JSON object alongside the standard members.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProblemDetail {
    /// URI reference identifying the problem class (JSON `type`).
    pub problem_type: String,
    /// Short, human-readable summary (JSON `title`).
    pub title: String,
    /// HTTP status code (JSON `status`); `0` is treated as unset.
    pub status: u16,
    /// Explanation specific to this occurrence (JSON `detail`).
    pub detail: String,
    /// URI of the request that produced the problem (JSON `instance`).
    pub instance: String,
    /// RFC 7807 §3.2 extension members. They are flattened into the
    /// JSON object alongside the standard members; standard members win
    /// on key collision.
    pub extensions: BTreeMap<String, Value>,
}

impl ProblemDetail {
    /// Builds a `ProblemDetail` with the standard members set.
    pub fn new(
        problem_type: impl Into<String>,
        title: impl Into<String>,
        status: u16,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            problem_type: problem_type.into(),
            title: title.into(),
            status,
            detail: detail.into(),
            instance: String::new(),
            extensions: BTreeMap::new(),
        }
    }

    /// Returns the problem with `instance` set to the given URI reference.
    #[must_use]
    pub fn with_instance(mut self, instance: impl Into<String>) -> Self {
        self.instance = instance.into();
        self
    }

    /// Sets an extension member and returns the problem.
    #[must_use]
    pub fn with(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.extensions.insert(key.into(), value.into());
        self
    }

    /// Returns a 400 Bad Request RFC 7807 envelope.
    pub fn bad_request(detail: impl Into<String>) -> Self {
        Self::new(TYPE_BAD_REQUEST, "Bad Request", 400, detail)
    }

    /// Returns a 401 Unauthorized RFC 7807 envelope.
    pub fn unauthorized(detail: impl Into<String>) -> Self {
        Self::new(TYPE_UNAUTHORIZED, "Unauthorized", 401, detail)
    }

    /// Returns a 403 Forbidden RFC 7807 envelope.
    pub fn forbidden(detail: impl Into<String>) -> Self {
        Self::new(TYPE_FORBIDDEN, "Forbidden", 403, detail)
    }

    /// Returns a 404 Not Found RFC 7807 envelope.
    pub fn not_found(detail: impl Into<String>) -> Self {
        Self::new(TYPE_NOT_FOUND, "Not Found", 404, detail)
    }

    /// Returns a 409 Conflict RFC 7807 envelope.
    pub fn conflict(detail: impl Into<String>) -> Self {
        Self::new(TYPE_CONFLICT, "Conflict", 409, detail)
    }

    /// Returns a 422 Unprocessable Entity RFC 7807 envelope.
    pub fn unprocessable(detail: impl Into<String>) -> Self {
        Self::new(TYPE_UNPROCESSABLE, "Unprocessable Entity", 422, detail)
    }

    /// Returns a 429 Too Many Requests RFC 7807 envelope.
    pub fn rate_limited(detail: impl Into<String>) -> Self {
        Self::new(TYPE_RATE_LIMITED, "Too Many Requests", 429, detail)
    }

    /// Returns a 500 Internal Server Error RFC 7807 envelope.
    pub fn internal(detail: impl Into<String>) -> Self {
        Self::new(TYPE_INTERNAL, "Internal Server Error", 500, detail)
    }

    /// Returns a 422 Validation Failed RFC 7807 envelope with
    /// [`TYPE_VALIDATION`] as the type URI — used when a domain object
    /// passes structural decoding but fails semantic validation.
    pub fn validation(detail: impl Into<String>) -> Self {
        Self::new(TYPE_VALIDATION, "Validation Failed", 422, detail)
    }
}

impl Serialize for ProblemDetail {
    /// Flattens [`ProblemDetail::extensions`] alongside the standard
    /// members, omitting empty standard members — byte-for-byte the
    /// shape the Go port emits (keys serialize in lexicographic order).
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut out: BTreeMap<String, Value> = BTreeMap::new();
        if !self.problem_type.is_empty() {
            out.insert("type".to_owned(), Value::String(self.problem_type.clone()));
        }
        if !self.title.is_empty() {
            out.insert("title".to_owned(), Value::String(self.title.clone()));
        }
        if self.status != 0 {
            out.insert("status".to_owned(), Value::from(self.status));
        }
        if !self.detail.is_empty() {
            out.insert("detail".to_owned(), Value::String(self.detail.clone()));
        }
        if !self.instance.is_empty() {
            out.insert("instance".to_owned(), Value::String(self.instance.clone()));
        }
        for (k, v) in &self.extensions {
            // Standard members win on collision (RFC 7807 §3.2).
            if !out.contains_key(k) {
                out.insert(k.clone(), v.clone());
            }
        }
        out.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ProblemDetail {
    /// Pulls out the standard members and stores the rest in
    /// [`ProblemDetail::extensions`]. A standard member of the wrong
    /// JSON type is left untouched in the extensions, exactly as the
    /// Go port behaves.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut raw = BTreeMap::<String, Value>::deserialize(deserializer)?;
        let mut pd = ProblemDetail::default();
        if let Some(v) = take_string(&mut raw, "type") {
            pd.problem_type = v;
        }
        if let Some(v) = take_string(&mut raw, "title") {
            pd.title = v;
        }
        if raw.get("status").is_some_and(Value::is_number) {
            if let Some(v) = raw.remove("status") {
                pd.status = v.as_f64().unwrap_or(0.0) as u16;
            }
        }
        if let Some(v) = take_string(&mut raw, "detail") {
            pd.detail = v;
        }
        if let Some(v) = take_string(&mut raw, "instance") {
            pd.instance = v;
        }
        pd.extensions = raw;
        Ok(pd)
    }
}

/// Removes `key` from `raw` and returns it only when its value is a
/// JSON string; non-string values stay put (they become extensions).
fn take_string(raw: &mut BTreeMap<String, Value>, key: &str) -> Option<String> {
    if raw.get(key).is_some_and(Value::is_string) {
        if let Some(Value::String(s)) = raw.remove(key) {
            return Some(s);
        }
    }
    None
}
