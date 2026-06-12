//! Ingestion endpoint tests, ported 1:1 from the Go module's
//! `web/handler_test.go` plus Rust-specific coverage. The router is
//! exercised in-process via `tower::ServiceExt::oneshot`.

mod common;

use std::sync::Arc;

use axum::body::Body;
use chrono::DateTime;
use http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use tower::ServiceExt as _;

use common::CaptureProcessor;
use firefly_kernel::FixedClock;
use firefly_testkit::{sign_hmac, sign_stripe, sign_twilio};
use firefly_webhooks::{web, HmacValidator, MemoryDlq, Pipeline, StripeValidator, TwilioValidator};

fn generic_pipeline(secret: &[u8]) -> (Arc<Pipeline>, Arc<MemoryDlq>, Arc<CaptureProcessor>) {
    let dlq = Arc::new(MemoryDlq::new());
    let pipeline = Arc::new(Pipeline::new(dlq.clone()));
    pipeline.register_validator(HmacValidator::new("generic", secret));
    let proc = CaptureProcessor::new("generic");
    pipeline.register_processor(proc.clone());
    (pipeline, dlq, proc)
}

async fn status_and_body(app: axum::Router, req: Request<Body>) -> (StatusCode, String) {
    let resp = app.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

// --- Go: TestIngestEndpointVerifiesAndDispatches ------------------------------

#[tokio::test]
async fn ingest_endpoint_verifies_and_dispatches() {
    let secret = b"s3cret";
    let (pipeline, _dlq, proc) = generic_pipeline(secret);
    let app = web::router(pipeline);

    let body: &[u8] = br#"{"x":1}"#;
    let sig = sign_hmac(secret, body);

    let req = Request::builder()
        .method("POST")
        .uri("/api/webhooks/generic")
        .header("X-Signature", &sig)
        .body(Body::from(body))
        .expect("request");
    let (status, text) = status_and_body(app.clone(), req).await;
    assert_eq!(status, StatusCode::ACCEPTED, "body: {text}");
    assert!(text.is_empty(), "202 has no body");
    assert_eq!(proc.hits(), 1);

    // Bad signature → 401.
    let req = Request::builder()
        .method("POST")
        .uri("/api/webhooks/generic")
        .header("X-Signature", "sha256=bad")
        .body(Body::from(body))
        .expect("request");
    let (status, text) = status_and_body(app.clone(), req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(text, "firefly/webhooks: signature mismatch\n");
    assert_eq!(proc.hits(), 1, "processor must not run on bad signature");

    // Unknown provider → 404.
    let req = Request::builder()
        .method("POST")
        .uri("/api/webhooks/missing")
        .body(Body::from(body))
        .expect("request");
    let (status, text) = status_and_body(app, req).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(text, "unknown provider\n");
}

// --- Rust-specific coverage --------------------------------------------------

#[tokio::test]
async fn ingest_endpoint_rejects_non_post() {
    let (pipeline, _dlq, _proc) = generic_pipeline(b"s3cret");
    let app = web::router(pipeline);

    for uri in ["/api/webhooks/generic", "/api/webhooks/"] {
        let req = Request::builder()
            .method("GET")
            .uri(uri)
            .body(Body::empty())
            .expect("request");
        let (status, text) = status_and_body(app.clone(), req).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "uri {uri}");
        assert_eq!(text, "POST only\n");
    }
}

#[tokio::test]
async fn ingest_endpoint_requires_a_provider_segment() {
    let (pipeline, _dlq, _proc) = generic_pipeline(b"s3cret");
    let app = web::router(pipeline);

    let req = Request::builder()
        .method("POST")
        .uri("/api/webhooks/")
        .body(Body::empty())
        .expect("request");
    let (status, text) = status_and_body(app, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(text, "provider required\n");
}

#[tokio::test]
async fn ingest_endpoint_dead_letters_and_returns_500_on_processor_error() {
    let secret = b"s3cret";
    let dlq = Arc::new(MemoryDlq::new());
    let pipeline = Arc::new(Pipeline::new(dlq.clone()));
    pipeline.register_validator(HmacValidator::new("generic", secret));
    pipeline.register_processor(CaptureProcessor::failing("generic", "boom"));
    let app = web::router(pipeline);

    let body: &[u8] = b"payload";
    let req = Request::builder()
        .method("POST")
        .uri("/api/webhooks/generic")
        .header("X-Signature", sign_hmac(secret, body))
        .body(Body::from(body))
        .expect("request");
    let (status, text) = status_and_body(app, req).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(text, "boom\n");

    let entries = dlq.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].err, "boom");
}

#[tokio::test]
async fn ingest_endpoint_builds_a_complete_inbound_event() {
    let secret = b"s3cret";
    let (pipeline, _dlq, proc) = generic_pipeline(secret);
    let app = web::router(pipeline);

    let body: &[u8] = br#"{"x":1}"#;
    let sig = sign_hmac(secret, body);
    let req = Request::builder()
        .method("POST")
        .uri("/api/webhooks/generic")
        .header("X-Signature", &sig)
        .header("X-Event-Type", "charge.succeeded")
        .body(Body::from(body))
        .expect("request");
    let (status, _) = status_and_body(app, req).await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let ev = proc.last().expect("processor captured the event");
    assert_eq!(ev.provider, "generic");
    assert_eq!(ev.event_type, "charge.succeeded");
    assert_eq!(ev.payload, body);
    // 12 random bytes, hex-encoded — Go's newID shape.
    assert_eq!(ev.id.len(), 24);
    assert!(ev.id.bytes().all(|b| b.is_ascii_hexdigit()));
    // Headers are flattened with Go's canonical MIME casing.
    assert_eq!(ev.headers.get("X-Signature"), Some(&sig));
    assert_eq!(
        ev.headers.get("X-Event-Type"),
        Some(&"charge.succeeded".to_owned())
    );
    assert!(ev.received_at > DateTime::UNIX_EPOCH);
}

#[tokio::test]
async fn ingest_endpoint_strips_host_from_inbound_headers() {
    // Go's net/http server promotes Host out of Request.Header (into
    // Request.Host) before the handler runs, so the Go port's
    // Inbound.headers never carries a "Host" entry. The Rust handler
    // must drop it too, or the persisted/dispatched event JSON
    // diverges from the Go wire shape.
    let secret = b"s3cret";
    let (pipeline, _dlq, proc) = generic_pipeline(secret);
    let app = web::router(pipeline);

    let body: &[u8] = br#"{"x":1}"#;
    let req = Request::builder()
        .method("POST")
        .uri("/api/webhooks/generic")
        .header("Host", "hooks.example.com")
        .header("X-Signature", sign_hmac(secret, body))
        .body(Body::from(body))
        .expect("request");
    let (status, text) = status_and_body(app, req).await;
    assert_eq!(status, StatusCode::ACCEPTED, "body: {text}");

    let ev = proc.last().expect("processor captured the event");
    assert!(
        !ev.headers.contains_key("Host"),
        "Inbound.headers must not carry Host: {:?}",
        ev.headers
    );
    assert!(ev.headers.contains_key("X-Signature"));
}

#[tokio::test]
async fn ingest_endpoint_accepts_stripe_testkit_signature() {
    let ts: i64 = 1_700_000_000;
    let clock = Arc::new(FixedClock(DateTime::from_timestamp(ts, 0).expect("ts")));

    let pipeline = Arc::new(Pipeline::new(Arc::new(MemoryDlq::new())));
    pipeline.register_validator(StripeValidator::new(b"whsec_test").with_clock(clock));
    let proc = CaptureProcessor::new("stripe");
    pipeline.register_processor(proc.clone());
    let app = web::router(pipeline);

    let body: &[u8] = br#"{"type":"charge.succeeded"}"#;
    let req = Request::builder()
        .method("POST")
        .uri("/api/webhooks/stripe")
        .header("Stripe-Signature", sign_stripe(b"whsec_test", body, ts))
        .body(Body::from(body))
        .expect("request");
    let (status, text) = status_and_body(app, req).await;
    assert_eq!(status, StatusCode::ACCEPTED, "body: {text}");
    assert_eq!(proc.hits(), 1);
}

#[tokio::test]
async fn ingest_endpoint_accepts_twilio_testkit_signature() {
    let url = "https://example.com/api/webhooks/twilio";
    let pipeline = Arc::new(Pipeline::new(Arc::new(MemoryDlq::new())));
    pipeline.register_validator(TwilioValidator::new(b"tok", url));
    let proc = CaptureProcessor::new("twilio");
    pipeline.register_processor(proc.clone());
    let app = web::router(pipeline);

    let body: &[u8] = b"From=%2B15017122661&Body=hello";
    let sig = sign_twilio(b"tok", url, &[("From", "+15017122661"), ("Body", "hello")]);
    let req = Request::builder()
        .method("POST")
        .uri("/api/webhooks/twilio")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("X-Twilio-Signature", &sig)
        .body(Body::from(body))
        .expect("request");
    let (status, text) = status_and_body(app, req).await;
    assert_eq!(status, StatusCode::ACCEPTED, "body: {text}");
    assert_eq!(proc.hits(), 1);
}
