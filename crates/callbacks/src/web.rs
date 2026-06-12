//! REST admin surface — the Rust spelling of the Go `callbacks/web`
//! sub-package.
//!
//! [`handler`] returns an axum [`Router`] exposing CRUD on
//! [`Target`]s and a read-only listing of [`Attempt`]s under the
//! `/callbacks` prefix:
//!
//! | Method   | Path                            | Response                              |
//! |----------|---------------------------------|---------------------------------------|
//! | `GET`    | `/callbacks/targets`            | `200` JSON array of targets           |
//! | `POST`   | `/callbacks/targets`            | `201` saved target (upsert)           |
//! | `GET`    | `/callbacks/targets/{id}`       | `200` target / `404`                  |
//! | `DELETE` | `/callbacks/targets/{id}`       | `204` / `404`                         |
//! | `GET`    | `/callbacks/attempts/{eventId}` | `200` JSON array (`null` when empty)  |
//!
//! Error responses reproduce Go's `http.Error` wire format exactly:
//! `text/plain; charset=utf-8`, `X-Content-Type-Options: nosniff`, and
//! the message followed by a newline. JSON responses reproduce Go's
//! `writeJSON` wire format: `application/json` and the document
//! terminated by `'\n'`, as `json.Encoder.Encode` emits.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::Router;
use http::{header, StatusCode};
use serde::Serialize;

use crate::interfaces::{Store, Target};

/// Shared router state: the pluggable persistence port.
type SharedStore = Arc<dyn Store>;

/// Returns an axum [`Router`] exposing CRUD on Targets and a read-only
/// listing of Attempts. The route prefix is `/callbacks` — Go's
/// `web.Handler(store)`.
pub fn handler(store: Arc<dyn Store>) -> Router {
    Router::new()
        .route(
            "/callbacks/targets",
            get(list_targets)
                .post(upsert_target)
                .fallback(method_not_allowed),
        )
        .route(
            "/callbacks/targets/:id",
            get(get_target)
                .delete(delete_target)
                .fallback(method_not_allowed),
        )
        // Go's mux serves this route for every method.
        .route("/callbacks/attempts/:event_id", any(list_attempts))
        .with_state(store)
}

/// `GET /callbacks/targets` — every registered target.
async fn list_targets(State(store): State<SharedStore>) -> Response {
    match store.list_targets().await {
        Ok(out) => write_json(StatusCode::OK, &out),
        Err(err) => http_error(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
    }
}

/// `POST /callbacks/targets` — upsert; `201` with the saved target.
/// Invalid JSON answers `400` with the decode error, like Go.
async fn upsert_target(State(store): State<SharedStore>, body: Bytes) -> Response {
    let target: Target = match serde_json::from_slice(&body) {
        Ok(t) => t,
        Err(err) => return http_error(StatusCode::BAD_REQUEST, &err.to_string()),
    };
    match store.upsert_target(target).await {
        Ok(saved) => write_json(StatusCode::CREATED, &saved),
        Err(err) => http_error(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
    }
}

/// `GET /callbacks/targets/{id}` — one target, or `404` carrying the
/// store error (`firefly/callbacks: not found`).
async fn get_target(State(store): State<SharedStore>, Path(id): Path<String>) -> Response {
    match store.get_target(&id).await {
        Ok(target) => write_json(StatusCode::OK, &target),
        Err(err) => http_error(StatusCode::NOT_FOUND, &err.to_string()),
    }
}

/// `DELETE /callbacks/targets/{id}` — `204`, or `404` carrying the
/// store error.
async fn delete_target(State(store): State<SharedStore>, Path(id): Path<String>) -> Response {
    match store.delete_target(&id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => http_error(StatusCode::NOT_FOUND, &err.to_string()),
    }
}

/// `GET /callbacks/attempts/{eventId}` — the audit trail of one event.
///
/// When the event has no recorded attempts the body is JSON `null`,
/// not `[]` — byte parity with the Go port, whose `MemoryStore`
/// returns a nil slice that `encoding/json` renders as `null`.
async fn list_attempts(State(store): State<SharedStore>, Path(event_id): Path<String>) -> Response {
    match store.list_attempts(&event_id).await {
        Ok(out) if out.is_empty() => write_json(StatusCode::OK, &serde_json::Value::Null),
        Ok(out) => write_json(StatusCode::OK, &out),
        Err(err) => http_error(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
    }
}

/// `405` for methods outside each route's set — Go's
/// `http.Error(w, "method not allowed", 405)`.
async fn method_not_allowed() -> Response {
    http_error(StatusCode::METHOD_NOT_ALLOWED, "method not allowed")
}

/// Reproduces Go's `writeJSON` (`json.NewEncoder(w).Encode(v)`):
/// `application/json` and the document terminated by `'\n'`, the
/// newline `json.Encoder.Encode` appends after every value.
fn write_json<T: Serialize>(status: StatusCode, value: &T) -> Response {
    match serde_json::to_vec(value) {
        Ok(mut body) => {
            // Go's json.Encoder terminates the document with '\n'.
            body.push(b'\n');
            (status, [(header::CONTENT_TYPE, "application/json")], body).into_response()
        }
        // Unreachable for the store's data types; Go's writeJSON also
        // swallows the encode error after the status line is out.
        Err(_) => status.into_response(),
    }
}

/// Reproduces Go's `http.Error`: plain-text content type, `nosniff`,
/// and the message terminated by a newline.
fn http_error(status: StatusCode, message: &str) -> Response {
    (
        status,
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        format!("{message}\n"),
    )
        .into_response()
}
