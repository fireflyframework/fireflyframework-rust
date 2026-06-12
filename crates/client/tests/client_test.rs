//! Integration tests for `firefly-client`, ported 1:1 from the Go
//! module's `client_test.go` and `grpc_soap_ws_test.go`, plus
//! Rust-specific coverage. Each test spawns a real axum server on a
//! random localhost port — the `httptest.NewServer` analog.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::Json;
use axum::http::{header, HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::Router;
use http::Method;
use serde::{Deserialize, Serialize};

use firefly_client::{
    new_grpc, new_rest, new_soap, new_websocket, ClientError, RestBuilder, NO_BODY,
};
use firefly_kernel::{with_correlation_id, ProblemDetail, PROBLEM_CONTENT_TYPE, TYPE_NOT_FOUND};

/// Binds an axum router on a random localhost port and returns the base
/// URL — the `httptest.NewServer` analog.
async fn spawn_server(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    format!("http://{addr}")
}

/// A fast retry schedule so retry tests stay well under 200 ms.
const FAST_BACKOFF: Duration = Duration::from_millis(1);

#[derive(Serialize, Deserialize)]
struct CreateUser {
    name: String,
}

#[derive(Serialize, Deserialize)]
struct User {
    id: String,
    name: String,
}

// --- Go: TestRESTHappyPath -------------------------------------------------

#[tokio::test]
async fn rest_happy_path() {
    let app = Router::new().route(
        "/users",
        post(|Json(input): Json<CreateUser>| async move {
            (
                StatusCode::CREATED,
                Json(User {
                    id: "u1".into(),
                    name: input.name,
                }),
            )
        }),
    );
    let base = spawn_server(app).await;

    let client = RestBuilder::new(&base)
        .with_header("X-Service", "orders")
        .build();
    let out: User = client
        .request(
            Method::POST,
            "/users",
            Some(&CreateUser {
                name: "alice".into(),
            }),
        )
        .await
        .expect("happy path");
    assert_eq!(out.id, "u1");
    assert_eq!(out.name, "alice");
}

// --- Go: TestRESTProblemDecode ---------------------------------------------

#[tokio::test]
async fn rest_problem_decode() {
    let app = Router::new().route(
        "/x",
        get(|| async {
            let pd = ProblemDetail::not_found("missing");
            (
                StatusCode::NOT_FOUND,
                [(header::CONTENT_TYPE, PROBLEM_CONTENT_TYPE)],
                serde_json::to_string(&pd).expect("encode problem"),
            )
        }),
    );
    let base = spawn_server(app).await;

    let client = RestBuilder::new(&base).with_retries(1).build();
    let err = client
        .send(Method::GET, "/x", NO_BODY)
        .await
        .expect_err("expected error");
    let fe = err.as_firefly().expect("FireflyError");
    assert_eq!(fe.status, 404);
    assert_eq!(fe.code, TYPE_NOT_FOUND);
    assert_eq!(fe.title, "Not Found");
    assert_eq!(fe.detail, "missing");
}

// --- Go: TestRESTRetriesOn500 ----------------------------------------------

#[tokio::test]
async fn rest_retries_on_500() {
    let hits = Arc::new(AtomicU32::new(0));
    let counter = hits.clone();
    let app = Router::new().route(
        "/x",
        get(move || {
            let counter = counter.clone();
            async move {
                let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
                if n < 3 {
                    (StatusCode::INTERNAL_SERVER_ERROR, String::new())
                } else {
                    (StatusCode::OK, r#"{"ok":true}"#.to_owned())
                }
            }
        }),
    );
    let base = spawn_server(app).await;

    #[derive(Deserialize)]
    struct Out {
        ok: bool,
    }
    let client = RestBuilder::new(&base)
        .with_retries(3)
        .with_backoff_base(FAST_BACKOFF)
        .build();
    let out: Out = client
        .request(Method::GET, "/x", NO_BODY)
        .await
        .expect("third attempt succeeds");
    assert_eq!(hits.load(Ordering::SeqCst), 3);
    assert!(out.ok);
}

// --- Go: TestNonRESTPlaceholders -------------------------------------------

#[test]
fn non_rest_placeholders() {
    assert!(matches!(
        new_soap("https://x").expect_err("NewSOAP"),
        ClientError::TransportNotRegistered
    ));
    assert!(matches!(
        new_grpc("dns:///x").expect_err("NewGRPC"),
        ClientError::TransportNotRegistered
    ));
    assert!(matches!(
        new_websocket("wss://x").expect_err("NewWebSocket"),
        ClientError::TransportNotRegistered
    ));
}

#[test]
fn transport_not_registered_message_matches_go() {
    assert_eq!(
        ClientError::TransportNotRegistered.to_string(),
        "firefly/client: transport adapter not registered"
    );
}

// --- Rust-specific coverage ------------------------------------------------

/// Captures the request headers seen by the server.
fn header_capture_app(seen: Arc<Mutex<Option<HeaderMap>>>) -> Router {
    Router::new().route(
        "/x",
        get(move |headers: HeaderMap| {
            let seen = seen.clone();
            async move {
                *seen.lock().expect("lock") = Some(headers);
                "{}"
            }
        }),
    )
}

#[tokio::test]
async fn correlation_id_propagates_from_task_local() {
    let seen = Arc::new(Mutex::new(None));
    let base = spawn_server(header_capture_app(seen.clone())).await;
    let client = RestBuilder::new(&base).build();

    with_correlation_id("abc123", async {
        client
            .send(Method::GET, "/x", NO_BODY)
            .await
            .expect("request");
    })
    .await;

    let headers = seen.lock().expect("lock").take().expect("headers");
    assert_eq!(
        headers
            .get("x-correlation-id")
            .and_then(|v| v.to_str().ok()),
        Some("abc123")
    );
}

#[tokio::test]
async fn no_correlation_header_without_scope() {
    let seen = Arc::new(Mutex::new(None));
    let base = spawn_server(header_capture_app(seen.clone())).await;
    let client = RestBuilder::new(&base).build();

    client
        .send(Method::GET, "/x", NO_BODY)
        .await
        .expect("request");

    let headers = seen.lock().expect("lock").take().expect("headers");
    assert!(headers.get("x-correlation-id").is_none());
}

#[tokio::test]
async fn default_headers_and_accept_are_sent() {
    let seen = Arc::new(Mutex::new(None));
    let base = spawn_server(header_capture_app(seen.clone())).await;
    let client = RestBuilder::new(&base)
        .with_header("X-Tenant", "acme")
        .build();

    client
        .send(Method::GET, "/x", NO_BODY)
        .await
        .expect("request");

    let headers = seen.lock().expect("lock").take().expect("headers");
    assert_eq!(
        headers.get("x-tenant").and_then(|v| v.to_str().ok()),
        Some("acme")
    );
    assert_eq!(
        headers.get("accept").and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
    // No body was sent, so no Content-Type — the Go contract.
    assert!(headers.get("content-type").is_none());
}

#[tokio::test]
async fn content_type_set_when_body_present() {
    let seen = Arc::new(Mutex::new(None));
    let base = spawn_server(header_capture_app(seen.clone())).await;
    let client = RestBuilder::new(&base).build();

    client
        .send(
            Method::GET,
            "/x",
            Some(&CreateUser {
                name: "alice".into(),
            }),
        )
        .await
        .expect("request");

    let headers = seen.lock().expect("lock").take().expect("headers");
    assert_eq!(
        headers.get("content-type").and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
}

#[tokio::test]
async fn non_problem_error_body_wraps_raw_detail() {
    let app = Router::new().route("/x", get(|| async { (StatusCode::BAD_REQUEST, "boom") }));
    let base = spawn_server(app).await;

    let client = RestBuilder::new(&base).with_retries(1).build();
    let err = client
        .send(Method::GET, "/x", NO_BODY)
        .await
        .expect_err("expected error");
    let fe = err.as_firefly().expect("FireflyError");
    assert_eq!(fe.status, 400);
    assert_eq!(fe.title, "Bad Request");
    assert_eq!(fe.code, "");
    assert_eq!(fe.detail, "boom");
    assert_eq!(err.status(), Some(400));
}

#[tokio::test]
async fn problem_extensions_become_fields() {
    let app = Router::new().route(
        "/x",
        get(|| async {
            let pd = ProblemDetail::bad_request("nope").with("field", "amount");
            (
                StatusCode::BAD_REQUEST,
                [(header::CONTENT_TYPE, PROBLEM_CONTENT_TYPE)],
                serde_json::to_string(&pd).expect("encode problem"),
            )
        }),
    );
    let base = spawn_server(app).await;

    let client = RestBuilder::new(&base).with_retries(1).build();
    let err = client
        .send(Method::GET, "/x", NO_BODY)
        .await
        .expect_err("expected error");
    let fe = err.as_firefly().expect("FireflyError");
    assert_eq!(fe.fields.get("field"), Some(&serde_json::json!("amount")));
}

#[tokio::test]
async fn retries_on_429() {
    let hits = Arc::new(AtomicU32::new(0));
    let counter = hits.clone();
    let app = Router::new().route(
        "/x",
        get(move || {
            let counter = counter.clone();
            async move {
                if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                    (StatusCode::TOO_MANY_REQUESTS, String::new())
                } else {
                    (StatusCode::OK, "{}".to_owned())
                }
            }
        }),
    );
    let base = spawn_server(app).await;

    let client = RestBuilder::new(&base)
        .with_retries(2)
        .with_backoff_base(FAST_BACKOFF)
        .build();
    client
        .send(Method::GET, "/x", NO_BODY)
        .await
        .expect("second attempt succeeds");
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn persistent_500_exhausts_attempts_and_returns_problem() {
    let hits = Arc::new(AtomicU32::new(0));
    let counter = hits.clone();
    let app = Router::new().route(
        "/x",
        get(move || {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                (StatusCode::INTERNAL_SERVER_ERROR, String::new())
            }
        }),
    );
    let base = spawn_server(app).await;

    let client = RestBuilder::new(&base)
        .with_retries(2)
        .with_backoff_base(FAST_BACKOFF)
        .build();
    let err = client
        .send(Method::GET, "/x", NO_BODY)
        .await
        .expect_err("expected error");
    assert_eq!(err.status(), Some(500));
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn zero_attempt_budget_is_exhausted_without_sending() {
    let hits = Arc::new(AtomicU32::new(0));
    let counter = hits.clone();
    let app = Router::new().route(
        "/x",
        get(move || {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                "{}"
            }
        }),
    );
    let base = spawn_server(app).await;

    let client = RestBuilder::new(&base).with_retries(0).build();
    let err = client
        .send(Method::GET, "/x", NO_BODY)
        .await
        .expect_err("expected error");
    assert!(matches!(err, ClientError::Exhausted(0)));
    assert_eq!(
        err.to_string(),
        "client: exhausted 0 attempts",
        "message matches Go's sentinel"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn network_errors_retry_then_surface_transport() {
    // Bind then drop a listener so the port is (almost certainly) dead.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local addr");
    drop(listener);

    let client = RestBuilder::new(format!("http://{addr}"))
        .with_retries(2)
        .with_backoff_base(FAST_BACKOFF)
        .build();
    let err = client
        .send(Method::GET, "/x", NO_BODY)
        .await
        .expect_err("expected error");
    assert!(matches!(err, ClientError::Transport(_)));
    assert!(err.as_firefly().is_none());
    assert_eq!(err.status(), None);
}

#[tokio::test]
async fn trailing_slash_on_base_url_is_trimmed() {
    let app = Router::new().route("/x", get(|| async { "{}" }));
    let base = spawn_server(app).await;

    let client = new_rest(format!("{base}//")).build();
    client
        .send(Method::GET, "/x", NO_BODY)
        .await
        .expect("trimmed base joins cleanly");
}

#[tokio::test]
async fn empty_success_body_decodes_to_unit() {
    let app = Router::new().route("/x", get(|| async { StatusCode::NO_CONTENT }));
    let base = spawn_server(app).await;

    let client = RestBuilder::new(&base).build();
    client
        .request::<(), ()>(Method::GET, "/x", NO_BODY)
        .await
        .expect("unit decode of empty body");
    let raw = client
        .send(Method::GET, "/x", NO_BODY)
        .await
        .expect("raw send");
    assert!(raw.is_empty());
}

#[tokio::test]
async fn send_returns_raw_success_bytes() {
    let app = Router::new().route("/x", get(|| async { r#"{"ok":true}"# }));
    let base = spawn_server(app).await;

    let client = RestBuilder::new(&base).build();
    let raw = client
        .send(Method::GET, "/x", NO_BODY)
        .await
        .expect("raw send");
    assert_eq!(raw, br#"{"ok":true}"#);
}

#[tokio::test]
async fn malformed_success_body_is_decode_error() {
    let app = Router::new().route("/x", get(|| async { "not json" }));
    let base = spawn_server(app).await;

    let client = RestBuilder::new(&base).build();
    let err = client
        .request::<(), serde_json::Value>(Method::GET, "/x", NO_BODY)
        .await
        .expect_err("expected decode error");
    assert!(matches!(err, ClientError::Decode(_)));
}

#[tokio::test]
async fn invalid_url_fails_fast() {
    let client = RestBuilder::new("not a url").with_retries(3).build();
    let err = client
        .send(Method::GET, "/x", NO_BODY)
        .await
        .expect_err("expected error");
    assert!(matches!(err, ClientError::InvalidUrl(_)));
}
