//! Internal RFC 7807 `application/problem+json` emission.
//!
//! `security` is a Wave-1 crate with no internal dependencies, so the
//! tiny slice of the kernel `ProblemDetail` it needs is reproduced
//! here. The JSON bytes match the Go port exactly: the Go `kernel`
//! marshals the envelope through a `map[string]any`, which Go sorts
//! alphabetically — so members are emitted in `detail`, `status`,
//! `title`, `type` order, empty members omitted.

use axum::body::Body;
use axum::response::Response;
use http::{header, StatusCode};

/// The IANA media type for `application/problem+json`.
pub(crate) const PROBLEM_CONTENT_TYPE: &str = "application/problem+json";

/// Canonical type URI for 401 problems (shared across all five ports).
pub(crate) const TYPE_UNAUTHORIZED: &str = "https://fireflyframework.org/problems/unauthorized";

/// Canonical type URI for 403 problems (shared across all five ports).
pub(crate) const TYPE_FORBIDDEN: &str = "https://fireflyframework.org/problems/forbidden";

/// Builds a 401 Unauthorized RFC 7807 response
/// (Go: `web.WriteProblem(w, kernel.ProblemUnauthorized(detail))`).
pub(crate) fn unauthorized(detail: &str) -> Response {
    write(
        StatusCode::UNAUTHORIZED,
        TYPE_UNAUTHORIZED,
        "Unauthorized",
        detail,
    )
}

/// Builds a 403 Forbidden RFC 7807 response
/// (Go: `web.WriteProblem(w, kernel.ProblemForbidden(detail))`).
pub(crate) fn forbidden(detail: &str) -> Response {
    write(StatusCode::FORBIDDEN, TYPE_FORBIDDEN, "Forbidden", detail)
}

/// Serializes the envelope. Members are inserted in alphabetical order
/// so the byte layout is stable regardless of the `serde_json` map
/// backend, matching Go's sorted map marshalling.
fn write(status: StatusCode, type_uri: &str, title: &str, detail: &str) -> Response {
    let mut body = serde_json::Map::new();
    if !detail.is_empty() {
        body.insert("detail".into(), detail.into());
    }
    body.insert("status".into(), u64::from(status.as_u16()).into());
    body.insert("title".into(), title.into());
    body.insert("type".into(), type_uri.into());
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, PROBLEM_CONTENT_TYPE)
        .body(Body::from(serde_json::Value::Object(body).to_string()))
        .expect("static problem response must build")
}
