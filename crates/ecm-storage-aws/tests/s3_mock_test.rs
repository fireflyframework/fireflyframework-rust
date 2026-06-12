//! End-to-end behavior tests for [`firefly_ecm_storage_aws::S3Store`] against an
//! in-process axum mock that stands in for S3 (port 0, path-style addressing
//! via `Config.endpoint`).
//!
//! These are the Rust analog of pyfly's `tests/ecm/test_aws_s3_behavior.py`:
//! pyfly injects a fake boto3 client and asserts the outbound call shape; here
//! the real reqwest + SigV4 path is exercised and the mock asserts the wire
//! contract — the HTTP method, the bucket-and-key path, the request body, and
//! the SigV4 `Authorization` / `x-amz-*` auth headers.

use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{OriginalUri, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::any;
use axum::Router;
use firefly_ecm::{bytes_reader, ContentStore, EcmError};
use firefly_ecm_storage_aws::{Config, S3Store};
use tokio::io::AsyncReadExt;

/// One recorded request the mock S3 server saw.
#[derive(Debug, Clone)]
struct Recorded {
    method: String,
    path: String,
    authorization: String,
    amz_date: String,
    content_sha256: String,
    body: Vec<u8>,
}

#[derive(Clone, Default)]
struct MockState {
    calls: Arc<Mutex<Vec<Recorded>>>,
    /// Body served back for GET requests.
    get_body: Arc<Mutex<Vec<u8>>>,
    /// When true, every request answers 404.
    not_found: Arc<Mutex<bool>>,
}

async fn handler(
    State(state): State<MockState>,
    OriginalUri(uri): OriginalUri,
    method: axum::http::Method,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, Vec<u8>) {
    let get = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    // Capture the *raw* (still percent-encoded) request path, so the test can
    // assert exactly what went over the wire (axum's `Path` extractor would
    // otherwise decode it).
    state.calls.lock().unwrap().push(Recorded {
        method: method.to_string(),
        path: uri.path().to_string(),
        authorization: get("authorization"),
        amz_date: get("x-amz-date"),
        content_sha256: get("x-amz-content-sha256"),
        body: body.to_vec(),
    });

    if *state.not_found.lock().unwrap() {
        return (StatusCode::NOT_FOUND, b"<Error>NoSuchKey</Error>".to_vec());
    }
    match method.as_str() {
        "GET" => (StatusCode::OK, state.get_body.lock().unwrap().clone()),
        "DELETE" => (StatusCode::NO_CONTENT, Vec::new()),
        _ => (StatusCode::OK, Vec::new()),
    }
}

/// Spawns the mock on port 0 and returns its base URL + shared state.
async fn spawn() -> (String, MockState) {
    let state = MockState::default();
    let app = Router::new()
        .route("/*key", any(handler))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), state)
}

fn store(endpoint: &str) -> S3Store {
    S3Store::new(Config {
        bucket: "my-bucket".into(),
        region: "eu-west-1".into(),
        access_key: "AKIDEXAMPLE".into(),
        secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
        endpoint: endpoint.into(),
        ..Default::default()
    })
    .unwrap()
}

/// Asserts the auth headers SigV4 must always set are present and well-formed.
fn assert_auth(rec: &Recorded) {
    assert!(
        rec.authorization
            .starts_with("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/"),
        "authorization missing/short: {}",
        rec.authorization
    );
    assert!(
        rec.authorization.contains("/eu-west-1/s3/aws4_request"),
        "scope must name region+service: {}",
        rec.authorization
    );
    assert!(
        rec.authorization
            .contains("SignedHeaders=host;x-amz-content-sha256;x-amz-date"),
        "signed headers list: {}",
        rec.authorization
    );
    assert!(
        rec.authorization.contains(", Signature="),
        "signature segment: {}",
        rec.authorization
    );
    assert!(
        rec.amz_date.ends_with('Z') && rec.amz_date.contains('T'),
        "x-amz-date shape: {}",
        rec.amz_date
    );
}

#[tokio::test]
async fn put_uploads_body_to_path_style_key_and_signs() {
    let (base, state) = spawn().await;
    let store = store(&base);
    let content = b"hello world";

    let n = store
        .put("doc-abc/v1", bytes_reader(content.to_vec()))
        .await
        .unwrap();

    assert_eq!(n, content.len() as i64);
    let calls = state.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let rec = &calls[0];
    assert_eq!(rec.method, "PUT");
    // Path-style addressing: /<bucket>/<key>.
    assert_eq!(rec.path, "/my-bucket/doc-abc/v1");
    assert_eq!(rec.body, content);
    // Payload hash header is the SHA-256 of the exact body.
    assert_eq!(
        rec.content_sha256,
        firefly_ecm_storage_aws::sigv4::sha256_hex(content)
    );
    assert_auth(rec);
}

#[tokio::test]
async fn get_returns_body_as_reader() {
    let (base, state) = spawn().await;
    *state.get_body.lock().unwrap() = b"PDF-BYTES".to_vec();
    let store = store(&base);

    let mut reader = store.get("doc-abc/v2").await.unwrap();
    let mut got = Vec::new();
    reader.read_to_end(&mut got).await.unwrap();

    assert_eq!(got, b"PDF-BYTES");
    let calls = state.calls.lock().unwrap();
    assert_eq!(calls[0].method, "GET");
    assert_eq!(calls[0].path, "/my-bucket/doc-abc/v2");
    // GET has an empty body, so the content hash is the empty-payload digest.
    assert_eq!(
        calls[0].content_sha256,
        firefly_ecm_storage_aws::sigv4::EMPTY_PAYLOAD_SHA256
    );
    assert_auth(&calls[0]);
}

#[tokio::test]
async fn get_missing_maps_to_not_found() {
    let (base, state) = spawn().await;
    *state.not_found.lock().unwrap() = true;
    let store = store(&base);

    match store.get("doc-abc/v9").await {
        Err(EcmError::NotFound) => {}
        Err(other) => panic!("expected NotFound, got {other}"),
        Ok(_) => panic!("expected NotFound, got Ok"),
    }
}

#[tokio::test]
async fn delete_issues_signed_delete() {
    let (base, state) = spawn().await;
    let store = store(&base);

    store.delete("doc-abc/v1").await.unwrap();

    let calls = state.calls.lock().unwrap();
    assert_eq!(calls[0].method, "DELETE");
    assert_eq!(calls[0].path, "/my-bucket/doc-abc/v1");
    assert_auth(&calls[0]);
}

#[tokio::test]
async fn delete_missing_is_not_an_error() {
    let (base, state) = spawn().await;
    *state.not_found.lock().unwrap() = true;
    let store = store(&base);

    // S3 returns 204/404 for a missing key; the port treats both as success.
    store.delete("doc-abc/gone").await.unwrap();
    assert_eq!(state.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn key_segments_are_percent_encoded() {
    let (base, state) = spawn().await;
    let store = store(&base);

    store
        .put("tenants/acme docs/doc-1/v1", bytes_reader(b"x".to_vec()))
        .await
        .unwrap();

    // The space is %20-encoded; the path slashes are preserved.
    assert_eq!(
        state.calls.lock().unwrap()[0].path,
        "/my-bucket/tenants/acme%20docs/doc-1/v1"
    );
}

#[tokio::test]
async fn non_success_status_surfaces_provider_error() {
    // A server that always answers 500 must surface a Provider error, not Ok.
    async fn boom() -> StatusCode {
        StatusCode::INTERNAL_SERVER_ERROR
    }
    let app = Router::new().route("/*key", any(boom));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let store = store(&format!("http://{addr}"));

    let err = store
        .put("k/v1", bytes_reader(b"x".to_vec()))
        .await
        .unwrap_err();
    assert!(matches!(err, EcmError::Provider(_)), "got {err}");
    assert!(err.to_string().contains("HTTP 500"), "{err}");
}

#[tokio::test]
async fn requires_complete_config() {
    // Missing credentials are rejected at construction.
    let err = S3Store::new(Config {
        bucket: "b".into(),
        region: "eu-west-1".into(),
        ..Default::default()
    })
    .unwrap_err();
    assert!(err.to_string().contains("access_key"), "{err}");

    let err = S3Store::new(Config::default()).unwrap_err();
    assert!(err.to_string().contains("bucket"), "{err}");
}
