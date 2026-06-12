//! The ingestion endpoint — `POST /api/webhooks/{provider}` — the Rust
//! spelling of the Go `webhooks/web` package.
//!
//! The handler:
//!
//! 1. Reads the body fully.
//! 2. Looks up the provider's [`Validator`](crate::Validator) —
//!    **404** if unknown.
//! 3. Verifies the signature — **401** on mismatch.
//! 4. Builds an [`Inbound`] and calls
//!    [`Pipeline::process`](crate::Pipeline::process).
//! 5. **202 Accepted** on success; **500** on processor error.
//!
//! Non-`POST` methods get **405** and a missing provider segment gets
//! **400**, matching the Go handler's `http.Error` responses (plain
//! text, trailing newline).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use chrono::Utc;
use http::{HeaderMap, Method, StatusCode};

use crate::core::Pipeline;
use crate::interfaces::Inbound;

/// Returns an axum [`Router`] mounting `POST /api/webhooks/{provider}`
/// over the given pipeline — the analog of Go's `web.Handler(p)`.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
///
/// use firefly_webhooks::{web, HmacValidator, MemoryDlq, Pipeline};
///
/// let pipeline = Arc::new(Pipeline::new(Arc::new(MemoryDlq::new())));
/// pipeline.register_validator(HmacValidator::new("generic", b"s3cret"));
/// let app: axum::Router = web::router(pipeline);
/// # let _ = app;
/// ```
pub fn router(pipeline: Arc<Pipeline>) -> Router {
    Router::new()
        .route("/api/webhooks/", any(missing_provider))
        .route("/api/webhooks/:provider", any(ingest))
        .with_state(pipeline)
}

/// `http.Error` analog: plain-text body with a trailing newline.
fn plain(status: StatusCode, msg: &str) -> Response {
    (status, format!("{msg}\n")).into_response()
}

/// Handles requests to the bare `/api/webhooks/` path, where Go's
/// prefix-trimming handler sees an empty provider segment.
async fn missing_provider(method: Method) -> Response {
    if method != Method::POST {
        return plain(StatusCode::METHOD_NOT_ALLOWED, "POST only");
    }
    plain(StatusCode::BAD_REQUEST, "provider required")
}

async fn ingest(
    State(pipeline): State<Arc<Pipeline>>,
    Path(provider): Path<String>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if method != Method::POST {
        return plain(StatusCode::METHOD_NOT_ALLOWED, "POST only");
    }

    let validators = pipeline.validators();
    let Some(validator) = validators.get(&provider) else {
        return plain(StatusCode::NOT_FOUND, "unknown provider");
    };
    if let Err(err) = validator.verify(&headers, &body) {
        return plain(StatusCode::UNAUTHORIZED, &err.to_string());
    }

    let ev = Inbound {
        id: new_id(),
        provider,
        event_type: headers
            .get("X-Event-Type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned(),
        headers: flatten_headers(&headers),
        payload: body.to_vec(),
        received_at: Utc::now(),
    };
    match pipeline.process(ev).await {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(err) if err.is_signature_mismatch() => {
            plain(StatusCode::UNAUTHORIZED, &err.to_string())
        }
        Err(err) => plain(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
    }
}

/// Flattens the header map to first-value-per-key with Go's canonical
/// MIME header casing, so the `Inbound.headers` JSON matches the Go
/// port byte-for-byte. `Host` is skipped: Go's `net/http` server
/// promotes it out of `Request.Header` (into `Request.Host`) before
/// the handler runs, so the Go port's map never carries a "Host"
/// entry — hyper keeps it in the map, hence the explicit exclusion.
fn flatten_headers(headers: &HeaderMap) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for key in headers.keys() {
        if key == http::header::HOST {
            continue;
        }
        if let Some(value) = headers.get(key).and_then(|v| v.to_str().ok()) {
            out.insert(canonical_header_key(key.as_str()), value.to_owned());
        }
    }
    out
}

/// Go's `textproto.CanonicalMIMEHeaderKey`: uppercase the first letter
/// and every letter following a hyphen, lowercase the rest. Keys
/// containing non-token bytes are returned unchanged.
pub(crate) fn canonical_header_key(key: &str) -> String {
    if !key.bytes().all(is_token_byte) {
        return key.to_owned();
    }
    let mut out = String::with_capacity(key.len());
    let mut upper = true;
    for b in key.bytes() {
        let c = if upper {
            b.to_ascii_uppercase()
        } else {
            b.to_ascii_lowercase()
        };
        out.push(char::from(c));
        upper = b == b'-';
    }
    out
}

/// Go's `validHeaderFieldByte`: RFC 7230 token characters.
fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

/// 12 random bytes, hex-encoded — Go's `newID`.
fn new_id() -> String {
    let bytes: [u8; 12] = rand::random();
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_header_key_matches_go_textproto() {
        assert_eq!(canonical_header_key("x-event-type"), "X-Event-Type");
        assert_eq!(canonical_header_key("content-type"), "Content-Type");
        assert_eq!(canonical_header_key("ACCEPT"), "Accept");
        assert_eq!(
            canonical_header_key("x-hub-signature-256"),
            "X-Hub-Signature-256"
        );
        // Non-token bytes leave the key untouched.
        assert_eq!(canonical_header_key("bad key"), "bad key");
        assert_eq!(canonical_header_key("weird:key"), "weird:key");
    }

    #[test]
    fn flatten_headers_strips_host_like_go_net_http() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::HOST,
            "hooks.example.com".parse().expect("host"),
        );
        headers.insert("x-event-type", "charge.succeeded".parse().expect("value"));
        let flat = flatten_headers(&headers);
        // Go's server moves Host into Request.Host before the handler
        // runs, so the flattened map must never contain it.
        assert!(!flat.contains_key("Host"), "headers: {flat:?}");
        assert_eq!(
            flat.get("X-Event-Type").map(String::as_str),
            Some("charge.succeeded")
        );
        assert_eq!(flat.len(), 1);
    }

    #[test]
    fn new_id_is_24_hex_chars() {
        let id = new_id();
        assert_eq!(id.len(), 24);
        assert!(id.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(new_id(), id);
    }
}
