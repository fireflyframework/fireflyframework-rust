//! End-to-end behavior tests for [`firefly_ecm_storage_azure::BlobStore`]
//! against an in-process axum mock standing in for Azure Blob Storage (port 0,
//! reached via `Config.endpoint`, like Azurite).
//!
//! These are the Rust analog of pyfly's `tests/ecm/test_azure_blob_behavior.py`:
//! pyfly injects a fake `BlobServiceClient` and asserts the outbound call
//! shape; here the real reqwest + Shared Key path is exercised and the mock
//! asserts the wire contract — the HTTP method, the container/blob path, the
//! body, the `x-ms-*` headers, and crucially that the `Authorization` header
//! carries a Shared Key signature the server can independently *recompute and
//! verify* from the canonical request.

use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{OriginalUri, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::any;
use axum::Router;
use firefly_ecm::{bytes_reader, ContentStore, EcmError};
use firefly_ecm_storage_azure::{sharedkey, BlobStore, Config};
use tokio::io::AsyncReadExt;

const ACCOUNT: &str = "devstoreaccount1";
// Public Azurite development account key — safe to embed in tests.
const KEY: &str =
    "Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==";
const CONTAINER: &str = "my-container";

/// One recorded request the mock saw, plus whether its Shared Key signature
/// verified server-side.
#[derive(Debug, Clone)]
struct Recorded {
    method: String,
    path: String,
    authorization: String,
    x_ms_date: String,
    x_ms_version: String,
    blob_type: String,
    body: Vec<u8>,
    signature_verified: bool,
}

#[derive(Clone, Default)]
struct MockState {
    calls: Arc<Mutex<Vec<Recorded>>>,
    get_body: Arc<Mutex<Vec<u8>>>,
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
    let path = uri.path().to_string();
    let authorization = get("authorization");
    let x_ms_date = get("x-ms-date");
    let x_ms_version = get("x-ms-version");
    let blob_type = get("x-ms-blob-type");

    // Re-derive the canonical request from the received headers/path and check
    // the Shared Key signature matches — this is the strong assertion that the
    // adapter signed *exactly* the request it sent.
    let signature_verified = verify_signature(
        method.as_str(),
        &path,
        &body,
        &x_ms_date,
        &x_ms_version,
        &blob_type,
        &authorization,
    );

    state.calls.lock().unwrap().push(Recorded {
        method: method.to_string(),
        path,
        authorization,
        x_ms_date,
        x_ms_version,
        blob_type,
        body: body.to_vec(),
        signature_verified,
    });

    if *state.not_found.lock().unwrap() {
        return (StatusCode::NOT_FOUND, b"BlobNotFound".to_vec());
    }
    match method.as_str() {
        "PUT" => (StatusCode::CREATED, Vec::new()),
        "GET" => (StatusCode::OK, state.get_body.lock().unwrap().clone()),
        "DELETE" => (StatusCode::ACCEPTED, Vec::new()),
        _ => (StatusCode::OK, Vec::new()),
    }
}

/// Rebuilds the canonical request from the incoming HTTP request and confirms
/// the `Authorization` header's signature matches a freshly computed one.
#[allow(clippy::too_many_arguments)]
fn verify_signature(
    method: &str,
    path: &str,
    body: &[u8],
    x_ms_date: &str,
    x_ms_version: &str,
    blob_type: &str,
    authorization: &str,
) -> bool {
    let is_put = method == "PUT";
    let content_length = if body.is_empty() {
        String::new()
    } else {
        body.len().to_string()
    };
    let content_type = if is_put {
        "application/octet-stream"
    } else {
        ""
    };
    let mut x_ms_headers = vec![
        sharedkey::Header::new("x-ms-date", x_ms_date),
        sharedkey::Header::new("x-ms-version", x_ms_version),
    ];
    if !blob_type.is_empty() {
        x_ms_headers.push(sharedkey::Header::new("x-ms-blob-type", blob_type));
    }
    // The mock is reached path-style (`/<container>/<blob>`); the canonical
    // resource always names the account.
    let canonical_resource = format!("/{ACCOUNT}{path}");
    let req = sharedkey::Request {
        method,
        content_length: &content_length,
        content_type,
        x_ms_headers,
        canonical_resource: &canonical_resource,
    };
    match sharedkey::sign(&req, ACCOUNT, KEY) {
        Ok((expected_authz, _, _)) => expected_authz == authorization,
        Err(_) => false,
    }
}

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

fn store(endpoint: &str) -> BlobStore {
    BlobStore::new(Config {
        account: ACCOUNT.into(),
        key: KEY.into(),
        container: CONTAINER.into(),
        endpoint: endpoint.into(),
        ..Default::default()
    })
    .unwrap()
}

fn assert_auth(rec: &Recorded) {
    assert!(
        rec.authorization.starts_with("SharedKey devstoreaccount1:"),
        "authorization scheme: {}",
        rec.authorization
    );
    assert_eq!(rec.x_ms_version, "2021-08-06", "x-ms-version header");
    assert!(
        rec.x_ms_date.ends_with("GMT"),
        "x-ms-date must be RFC1123 GMT: {}",
        rec.x_ms_date
    );
    assert!(
        rec.signature_verified,
        "server could not recompute the Shared Key signature for {} {}",
        rec.method, rec.path
    );
}

#[tokio::test]
async fn put_uploads_block_blob_and_signs() {
    let (base, state) = spawn().await;
    let store = store(&base);
    let content = b"hello world";

    let n = store
        .put("doc-xyz/v1", bytes_reader(content.to_vec()))
        .await
        .unwrap();

    assert_eq!(n, content.len() as i64);
    let calls = state.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let rec = &calls[0];
    assert_eq!(rec.method, "PUT");
    assert_eq!(rec.path, "/my-container/doc-xyz/v1");
    assert_eq!(rec.body, content);
    // PUT block blob sets x-ms-blob-type: BlockBlob.
    assert_eq!(rec.blob_type, "BlockBlob");
    assert_auth(rec);
}

#[tokio::test]
async fn get_returns_body_as_reader() {
    let (base, state) = spawn().await;
    *state.get_body.lock().unwrap() = b"PDF-BYTES".to_vec();
    let store = store(&base);

    let mut reader = store.get("doc-xyz/v2").await.unwrap();
    let mut got = Vec::new();
    reader.read_to_end(&mut got).await.unwrap();

    assert_eq!(got, b"PDF-BYTES");
    let calls = state.calls.lock().unwrap();
    assert_eq!(calls[0].method, "GET");
    assert_eq!(calls[0].path, "/my-container/doc-xyz/v2");
    // GET carries no blob-type header.
    assert_eq!(calls[0].blob_type, "");
    assert_auth(&calls[0]);
}

#[tokio::test]
async fn get_missing_maps_to_not_found() {
    let (base, state) = spawn().await;
    *state.not_found.lock().unwrap() = true;
    let store = store(&base);

    match store.get("doc-xyz/v9").await {
        Err(EcmError::NotFound) => {}
        Err(other) => panic!("expected NotFound, got {other}"),
        Ok(_) => panic!("expected NotFound, got Ok"),
    }
}

#[tokio::test]
async fn delete_issues_signed_delete() {
    let (base, state) = spawn().await;
    let store = store(&base);

    store.delete("doc-xyz/v1").await.unwrap();

    let calls = state.calls.lock().unwrap();
    assert_eq!(calls[0].method, "DELETE");
    assert_eq!(calls[0].path, "/my-container/doc-xyz/v1");
    assert_auth(&calls[0]);
}

#[tokio::test]
async fn delete_missing_is_not_an_error() {
    let (base, state) = spawn().await;
    *state.not_found.lock().unwrap() = true;
    let store = store(&base);

    store.delete("doc-xyz/gone").await.unwrap();
    assert_eq!(state.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn blob_name_segments_are_percent_encoded() {
    let (base, state) = spawn().await;
    let store = store(&base);

    store
        .put("tenants/acme docs/doc-1/v1", bytes_reader(b"x".to_vec()))
        .await
        .unwrap();

    let calls = state.calls.lock().unwrap();
    assert_eq!(calls[0].path, "/my-container/tenants/acme%20docs/doc-1/v1");
    // The signature must still verify over the encoded canonical resource.
    assert!(calls[0].signature_verified);
}

#[tokio::test]
async fn non_success_status_surfaces_provider_error() {
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
async fn requires_complete_config_and_valid_key() {
    let err = BlobStore::new(Config {
        account: "a".into(),
        container: "c".into(),
        ..Default::default()
    })
    .unwrap_err();
    assert!(err.to_string().contains("key"), "{err}");

    let err = BlobStore::new(Config::default()).unwrap_err();
    assert!(err.to_string().contains("account"), "{err}");

    // A non-base64 key is accepted at construction but fails at sign time.
    let (base, _state) = spawn().await;
    let store = BlobStore::new(Config {
        account: ACCOUNT.into(),
        key: "not!base64!".into(),
        container: CONTAINER.into(),
        endpoint: base,
        ..Default::default()
    })
    .unwrap();
    let err = store
        .put("k/v1", bytes_reader(b"x".to_vec()))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("sign"), "{err}");
}
