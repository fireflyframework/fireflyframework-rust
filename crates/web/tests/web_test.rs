//! In-process HTTP tests for the firefly-web middleware stack, ported
//! 1:1 from the Go module's `web_test.go` (plus Rust-specific cases),
//! driven through `tower::ServiceExt::oneshot` — no sockets.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Extension, Router};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use firefly_kernel::{
    FireflyError, ProblemDetail, HEADER_CORRELATION_ID, HEADER_IDEMPOTENCY_KEY,
    PROBLEM_CONTENT_TYPE, TYPE_BAD_REQUEST, TYPE_IDEMPOTENCY, TYPE_INTERNAL, TYPE_NOT_FOUND,
};
use firefly_web::{
    error_response, mask_map, mask_pii, problem_response, CorrelationId, CorrelationLayer,
    IdempotencyConfig, IdempotencyLayer, IdempotencyRecord, IdempotencyStore,
    MemoryIdempotencyStore, ProblemLayer, WebResult,
};
use http::{header, HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

/// Sends a request through the router and returns status, headers, and
/// collected body bytes.
async fn send(app: Router, req: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
    let response = app.oneshot(req).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, body.to_vec())
}

fn get_req(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

// ----- Go: TestProblemMiddlewareCatchesPanic -----

#[tokio::test]
async fn problem_layer_catches_panic() {
    async fn handler() -> &'static str {
        panic!("boom")
    }
    let app = Router::new()
        .route("/x", get(handler))
        .layer(ProblemLayer::new());

    let (status, headers, body) = send(app, get_req("/x")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        headers.get(header::CONTENT_TYPE).unwrap(),
        PROBLEM_CONTENT_TYPE
    );
    let pd: ProblemDetail = serde_json::from_slice(&body).unwrap();
    assert_eq!(pd.problem_type, TYPE_INTERNAL);
    assert_eq!(pd.detail, "boom");
}

// ----- Go: TestErrorHandlerRendersFireflyError -----

#[tokio::test]
async fn web_result_renders_firefly_error() {
    async fn handler() -> WebResult<&'static str> {
        Err(FireflyError::not_found("missing").into())
    }
    let app = Router::new().route("/x", get(handler));

    let (status, headers, body) = send(app, get_req("/x")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        headers.get(header::CONTENT_TYPE).unwrap(),
        PROBLEM_CONTENT_TYPE
    );
    let pd: ProblemDetail = serde_json::from_slice(&body).unwrap();
    assert_eq!(pd.problem_type, TYPE_NOT_FOUND);
    assert_eq!(pd.detail, "missing");
}

// ----- Go: TestCorrelationMiddleware -----

#[tokio::test]
async fn correlation_layer_generates_and_echoes() {
    let captured = Arc::new(Mutex::new(String::new()));
    let captured_clone = Arc::clone(&captured);
    let app = Router::new()
        .route(
            "/x",
            get(move || {
                let captured = Arc::clone(&captured_clone);
                async move {
                    *captured.lock().unwrap() =
                        firefly_kernel::correlation_id().unwrap_or_default();
                    StatusCode::OK
                }
            }),
        )
        .layer(CorrelationLayer::new());

    // Generated when missing.
    let (status, headers, _) = send(app.clone(), get_req("/x")).await;
    assert_eq!(status, StatusCode::OK);
    let generated = captured.lock().unwrap().clone();
    assert!(!generated.is_empty(), "correlation id not set");
    assert_eq!(
        headers
            .get(HEADER_CORRELATION_ID)
            .unwrap()
            .to_str()
            .unwrap(),
        generated,
        "response header mismatch"
    );

    // Echoed when provided.
    let req = Request::builder()
        .uri("/x")
        .header(HEADER_CORRELATION_ID, "abc-xyz")
        .body(Body::empty())
        .unwrap();
    let (_, headers, _) = send(app, req).await;
    assert_eq!(headers.get(HEADER_CORRELATION_ID).unwrap(), "abc-xyz");
    assert_eq!(*captured.lock().unwrap(), "abc-xyz");
}

// Regression (Go parity): the X-Correlation-Id header must survive the
// panic→500 path. Go's CorrelationMiddleware stages the header on the
// shared response map before invoking next, so the 500 written by the
// recover middleware still carries it.

#[tokio::test]
async fn correlation_header_survives_panic() {
    async fn handler() -> &'static str {
        panic!("boom")
    }
    let app = Router::new().route("/x", get(handler)).layer(
        tower::ServiceBuilder::new()
            .layer(ProblemLayer::new())
            .layer(CorrelationLayer::new()),
    );

    // A supplied id is echoed on the recovered 500.
    let req = Request::builder()
        .uri("/x")
        .header(HEADER_CORRELATION_ID, "abc-123")
        .body(Body::empty())
        .unwrap();
    let (status, headers, body) = send(app.clone(), req).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        headers.get(HEADER_CORRELATION_ID).unwrap(),
        "abc-123",
        "correlation header lost on panic path"
    );
    let pd: ProblemDetail = serde_json::from_slice(&body).unwrap();
    assert_eq!(pd.problem_type, TYPE_INTERNAL);
    assert_eq!(pd.detail, "boom");

    // A generated id is present too.
    let (status, headers, _) = send(app, get_req("/x")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let generated = headers.get(HEADER_CORRELATION_ID).expect("header missing");
    assert!(!generated.to_str().unwrap().is_empty());
}

// Rust-specific: the id is also exposed via request extensions.

#[tokio::test]
async fn correlation_id_available_as_extension() {
    async fn handler(Extension(CorrelationId(id)): Extension<CorrelationId>) -> String {
        id
    }
    let app = Router::new()
        .route("/x", get(handler))
        .layer(CorrelationLayer::new());

    let req = Request::builder()
        .uri("/x")
        .header(HEADER_CORRELATION_ID, "ext-123")
        .body(Body::empty())
        .unwrap();
    let (status, _, body) = send(app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"ext-123");
}

// ----- Go: TestIdempotencyReplaysAndConflicts -----

fn counting_app(calls: Arc<AtomicUsize>) -> Router {
    Router::new()
        .route(
            "/orders",
            post(move || {
                let calls = Arc::clone(&calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::CREATED,
                        [(header::CONTENT_TYPE, "application/json")],
                        r#"{"id":"42"}"#,
                    )
                }
            }),
        )
        .layer(IdempotencyLayer::default())
}

fn keyed_post(body: &str) -> Request<Body> {
    Request::builder()
        .method(http::Method::POST)
        .uri("/orders")
        .header(HEADER_IDEMPOTENCY_KEY, "abc")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_owned()))
        .unwrap()
}

#[tokio::test]
async fn idempotency_replays_and_conflicts() {
    let calls = Arc::new(AtomicUsize::new(0));
    let app = counting_app(Arc::clone(&calls));

    // First call hits inner.
    let (status, _, _) = send(app.clone(), keyed_post(r#"{"x":1}"#)).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Second call replays, inner not hit again.
    let (status, headers, body) = send(app.clone(), keyed_post(r#"{"x":1}"#)).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "inner hit on replay");
    assert_eq!(
        headers.get("Idempotent-Replay").unwrap(),
        "true",
        "replay header missing"
    );
    assert!(String::from_utf8_lossy(&body).contains(r#""id":"42""#));
    // The captured Content-Type is replayed too.
    assert_eq!(
        headers.get(header::CONTENT_TYPE).unwrap(),
        "application/json"
    );

    // Conflict — same key, different body.
    let (status, _, body) = send(app, keyed_post(r#"{"x":2}"#)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let pd: ProblemDetail = serde_json::from_slice(&body).unwrap();
    assert_eq!(pd.problem_type, TYPE_IDEMPOTENCY);
}

// Rust-specific: requests without a key, or with non-configured methods,
// pass straight through.

#[tokio::test]
async fn idempotency_passes_through_without_key() {
    let calls = Arc::new(AtomicUsize::new(0));
    let app = counting_app(Arc::clone(&calls));

    let unkeyed = || {
        Request::builder()
            .method(http::Method::POST)
            .uri("/orders")
            .body(Body::from(r#"{"x":1}"#))
            .unwrap()
    };
    send(app.clone(), unkeyed()).await;
    send(app, unkeyed()).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn idempotency_ignores_unconfigured_methods() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_clone = Arc::clone(&calls);
    let app = Router::new()
        .route(
            "/orders",
            get(move || {
                let calls = Arc::clone(&calls_clone);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    StatusCode::OK
                }
            }),
        )
        .layer(IdempotencyLayer::default());

    let keyed_get = || {
        Request::builder()
            .uri("/orders")
            .header(HEADER_IDEMPOTENCY_KEY, "abc")
            .body(Body::empty())
            .unwrap()
    };
    send(app.clone(), keyed_get()).await;
    send(app, keyed_get()).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2, "GET must not be replayed");
}

// Regression (Go parity): the first keyed response must stream through
// to the client as the handler produces it — Go's captureWriter
// forwards every Write immediately while copying it — rather than being
// fully buffered before the first byte is sent. The fully streamed body
// must still be captured and replayed.

#[tokio::test]
async fn idempotency_streams_response_body_through() {
    let (tx, rx) = futures::channel::mpsc::unbounded::<Result<Vec<u8>, std::io::Error>>();
    let rx_slot = Arc::new(Mutex::new(Some(rx)));
    let rx_clone = Arc::clone(&rx_slot);
    let app = Router::new()
        .route(
            "/orders",
            post(move || {
                let rx = rx_clone.lock().unwrap().take();
                async move {
                    match rx {
                        Some(rx) => (StatusCode::CREATED, Body::from_stream(rx)).into_response(),
                        // A replay must not reach the handler again.
                        None => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                    }
                }
            }),
        )
        .layer(IdempotencyLayer::default());

    tx.unbounded_send(Ok(b"hello ".to_vec())).unwrap();

    // With a buffered implementation, the response head would not
    // resolve until the stream closes — so guard with timeouts.
    let res = tokio::time::timeout(
        Duration::from_secs(2),
        app.clone().oneshot(keyed_post(r#"{"x":1}"#)),
    )
    .await
    .expect("response head must arrive while the body stream is still open")
    .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);

    let mut body = res.into_body();
    let first = tokio::time::timeout(Duration::from_secs(2), body.frame())
        .await
        .expect("first chunk must arrive while the body stream is still open")
        .unwrap()
        .unwrap();
    assert_eq!(first.data_ref().unwrap().as_ref(), b"hello ");

    tx.unbounded_send(Ok(b"world".to_vec())).unwrap();
    drop(tx);
    let rest = body.collect().await.unwrap().to_bytes();
    assert_eq!(rest.as_ref(), b"world");

    // The streamed response was captured in full and now replays.
    let (status, headers, body) = send(app, keyed_post(r#"{"x":1}"#)).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(headers.get("Idempotent-Replay").unwrap(), "true");
    assert_eq!(body, b"hello world");
}

// Rust-specific: non-2xx responses are not persisted, so the inner
// handler runs again on retry.

#[tokio::test]
async fn idempotency_does_not_store_failures() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_clone = Arc::clone(&calls);
    let app = Router::new()
        .route(
            "/orders",
            post(move || {
                let calls = Arc::clone(&calls_clone);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    StatusCode::BAD_REQUEST
                }
            }),
        )
        .layer(IdempotencyLayer::default());

    let (status, _, _) = send(app.clone(), keyed_post(r#"{"x":1}"#)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, headers, _) = send(app, keyed_post(r#"{"x":1}"#)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(headers.get("Idempotent-Replay").is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

// Rust-specific: memory store TTL expiry and zero-TTL (never expires).

#[tokio::test]
async fn memory_store_expires_entries() {
    let store = MemoryIdempotencyStore::new();
    let rec = sample_record();

    store
        .put("short", rec.clone(), Duration::from_millis(30))
        .await
        .unwrap();
    store.put("forever", rec, Duration::ZERO).await.unwrap();

    assert!(store.get("short").await.unwrap().is_some());
    assert!(store.get("forever").await.unwrap().is_some());
    assert!(store.get("absent").await.unwrap().is_none());

    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(store.get("short").await.unwrap().is_none(), "not expired");
    assert!(store.get("forever").await.unwrap().is_some(), "expired");
}

// Rust-specific: the record JSON matches the Go wire shape (field names,
// base64 body) so shared stores replay across runtimes.

fn sample_record() -> IdempotencyRecord {
    IdempotencyRecord {
        status_code: 201,
        headers: [("Content-Type".to_owned(), "application/json".to_owned())]
            .into_iter()
            .collect(),
        body: br#"{"id":"42"}"#.to_vec(),
        body_hash: "bh".to_owned(),
        stored_at: "2026-06-12T10:00:00Z".parse().unwrap(),
        request_hash: "rh".to_owned(),
    }
}

#[test]
fn idempotency_record_serializes_like_go() {
    let rec = sample_record();
    let json = serde_json::to_value(&rec).unwrap();
    assert_eq!(json["status"], 201);
    assert_eq!(json["headers"]["Content-Type"], "application/json");
    assert_eq!(json["body"], STANDARD.encode(br#"{"id":"42"}"#));
    assert_eq!(json["bodyHash"], "bh");
    assert_eq!(json["requestHash"], "rh");
    assert!(json["storedAt"]
        .as_str()
        .unwrap()
        .starts_with("2026-06-12T10:00:00"));

    let roundtrip: IdempotencyRecord = serde_json::from_value(json).unwrap();
    assert_eq!(roundtrip, rec);
}

// ----- Go: TestPIIMasking -----

#[test]
fn pii_masking() {
    let input = "user a@b.co with iban GB82WEST12345698765432 and card 4539 1488 0343 6467 phone +34911223344";
    let out = mask_pii(input);
    for kind in ["email", "iban", "card", "phone"] {
        assert!(
            out.contains(&format!("[REDACTED:{kind}]")),
            "missing redaction for {kind} in {out:?}"
        );
    }

    let m = serde_json::json!({
        "email": "alice@example.com",
        "password": "hunter2",
        "nested": {"token": "tok", "note": "call +34911223344"},
        "untouched": 42,
    });
    let masked = mask_map(m.as_object().unwrap());
    assert_eq!(masked["password"], "[REDACTED]", "password not redacted");
    assert_eq!(
        masked["nested"]["token"], "[REDACTED]",
        "nested token not redacted"
    );
    assert!(
        masked["nested"]["note"]
            .as_str()
            .unwrap()
            .contains("[REDACTED:phone]"),
        "nested phone not redacted"
    );
    assert_eq!(masked["email"], "[REDACTED:email]");
    assert_eq!(masked["untouched"], 42);
}

// Regression (Go parity): Go's RE2 gives `\b`/`\d` ASCII semantics, so
// a card/phone/IBAN adjacent to a non-ASCII letter is still masked, and
// non-ASCII digit runs are not mistaken for card numbers. The Rust
// patterns must behave identically (`(?-u)`), or full PII leaks into
// logs for international free-form text.

#[test]
fn pii_masking_uses_ascii_semantics_like_go() {
    // Unicode letters adjacent to the number must not suppress the
    // word boundary (Go masks both of these).
    assert_eq!(
        mask_pii("nº4111111111111111 done"),
        "nº[REDACTED:card] done",
        "card adjacent to non-ASCII letter must be masked"
    );
    assert_eq!(
        mask_pii("tel:+34911223344é end"),
        "tel:[REDACTED:phone]é end",
        "phone adjacent to non-ASCII letter must be masked"
    );
    assert_eq!(
        mask_pii("éGB82WEST12345698765432"),
        "é[REDACTED:iban]",
        "iban adjacent to non-ASCII letter must be masked"
    );

    // Arabic-Indic digits are not `\d` in Go; they must stay unmasked.
    let arabic = "رقم ٠١٢٣٤٥٦٧٨٩٠١٢٣٤٥";
    assert_eq!(
        mask_pii(arabic),
        arabic,
        "non-ASCII digit runs must not be redacted"
    );
}

// Rust-specific: problem_response wire bytes match the Go WriteProblem
// output exactly (compact JSON, sorted keys, trailing newline).

#[tokio::test]
async fn problem_response_matches_go_bytes() {
    let res = problem_response(&ProblemDetail::internal("boom"));
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        res.headers().get(header::CONTENT_TYPE).unwrap(),
        PROBLEM_CONTENT_TYPE
    );
    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        body.as_ref(),
        concat!(
            r#"{"detail":"boom","status":500,"title":"Internal Server Error","#,
            r#""type":"https://fireflyframework.org/problems/internal-error"}"#,
            "\n"
        )
        .as_bytes()
    );
}

// Rust-specific: a zero status defaults to 500, as in Go's WriteProblem.

#[test]
fn problem_response_defaults_zero_status_to_500() {
    let pd = ProblemDetail::new(TYPE_BAD_REQUEST, "Bad Request", 0, "x");
    let res = problem_response(&pd);
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

// Rust-specific: error_response renders FireflyError with its status and
// any other error as a 500 internal (Go's WriteError + kernel.AsProblem).

#[tokio::test]
async fn error_response_converts_via_as_problem() {
    let fe = FireflyError::not_found("missing");
    let res = error_response(&fe);
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let pd: ProblemDetail = serde_json::from_slice(&body).unwrap();
    assert_eq!(pd.problem_type, TYPE_NOT_FOUND);

    let io = std::io::Error::other("disk gone");
    let res = error_response(&io);
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let pd: ProblemDetail = serde_json::from_slice(&body).unwrap();
    assert_eq!(pd.problem_type, TYPE_INTERNAL);
    assert_eq!(pd.detail, "disk gone");
}

// Rust-specific: the full canonical chain — Problem(Correlation(
// Idempotency(router))) — works end to end, replaying with the header.

#[tokio::test]
async fn canonical_chain_composes() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_clone = Arc::clone(&calls);
    let app = Router::new()
        .route(
            "/orders",
            post(move || {
                let calls = Arc::clone(&calls_clone);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    assert!(firefly_kernel::correlation_id().is_some());
                    (StatusCode::CREATED, r#"{"id":"42"}"#)
                }
            }),
        )
        .layer(
            tower::ServiceBuilder::new()
                .layer(ProblemLayer::new())
                .layer(CorrelationLayer::new())
                .layer(IdempotencyLayer::new(IdempotencyConfig::default())),
        );

    let (status, headers, _) = send(app.clone(), keyed_post(r#"{"x":1}"#)).await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(headers.get(HEADER_CORRELATION_ID).is_some());

    let (status, headers, body) = send(app, keyed_post(r#"{"x":1}"#)).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(headers.get("Idempotent-Replay").unwrap(), "true");
    assert!(String::from_utf8_lossy(&body).contains(r#""id":"42""#));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
