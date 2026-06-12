// Copyright 2026 Firefly Software Foundation.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

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
    query: String,
    authorization: String,
    x_ms_date: String,
    x_ms_version: String,
    blob_type: String,
    copy_source: String,
    body: Vec<u8>,
    signature_verified: bool,
}

#[derive(Clone, Default)]
struct MockState {
    calls: Arc<Mutex<Vec<Recorded>>>,
    get_body: Arc<Mutex<Vec<u8>>>,
    /// EnumerationResults XML served for List Blobs.
    list_xml: Arc<Mutex<String>>,
    not_found: Arc<Mutex<bool>>,
}

async fn handler(
    State(state): State<MockState>,
    OriginalUri(uri): OriginalUri,
    method: axum::http::Method,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let get = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    let path = uri.path().to_string();
    let query = uri.query().unwrap_or("").to_string();
    let authorization = get("authorization");
    let x_ms_date = get("x-ms-date");
    let x_ms_version = get("x-ms-version");
    let blob_type = get("x-ms-blob-type");
    let copy_source = get("x-ms-copy-source");

    // Re-derive the canonical request from the received headers/path/query and
    // check the Shared Key signature matches — the strong assertion that the
    // adapter signed *exactly* the request it sent.
    let signature_verified = verify_signature(VerifyInput {
        method: method.as_str(),
        path: &path,
        query: &query,
        body: &body,
        x_ms_date: &x_ms_date,
        x_ms_version: &x_ms_version,
        blob_type: &blob_type,
        copy_source: &copy_source,
        authorization: &authorization,
    });

    state.calls.lock().unwrap().push(Recorded {
        method: method.to_string(),
        path: path.clone(),
        query: query.clone(),
        authorization,
        x_ms_date,
        x_ms_version,
        blob_type,
        copy_source: copy_source.clone(),
        body: body.to_vec(),
        signature_verified,
    });

    let mut out_headers = HeaderMap::new();
    if *state.not_found.lock().unwrap() {
        return (StatusCode::NOT_FOUND, out_headers, b"BlobNotFound".to_vec());
    }
    match method.as_str() {
        // List Blobs: GET with comp=list query.
        "GET" if query.contains("comp=list") => (
            StatusCode::OK,
            out_headers,
            state.list_xml.lock().unwrap().clone().into_bytes(),
        ),
        // Copy Blob: PUT with x-ms-copy-source → 202 Accepted.
        "PUT" if !copy_source.is_empty() => {
            out_headers.insert("x-ms-copy-status", "success".parse().unwrap());
            (StatusCode::ACCEPTED, out_headers, Vec::new())
        }
        "PUT" => (StatusCode::CREATED, out_headers, Vec::new()),
        "HEAD" => {
            out_headers.insert("content-length", "9".parse().unwrap());
            out_headers.insert("content-type", "application/pdf".parse().unwrap());
            out_headers.insert("etag", "\"0x8DA\"".parse().unwrap());
            (StatusCode::OK, out_headers, Vec::new())
        }
        "GET" => (
            StatusCode::OK,
            out_headers,
            state.get_body.lock().unwrap().clone(),
        ),
        "DELETE" => (StatusCode::ACCEPTED, out_headers, Vec::new()),
        _ => (StatusCode::OK, out_headers, Vec::new()),
    }
}

/// The fields the mock needs to recompute a Shared Key signature.
struct VerifyInput<'a> {
    method: &'a str,
    path: &'a str,
    query: &'a str,
    body: &'a [u8],
    x_ms_date: &'a str,
    x_ms_version: &'a str,
    blob_type: &'a str,
    copy_source: &'a str,
    authorization: &'a str,
}

/// Rebuilds the canonical request from the incoming HTTP request and confirms
/// the `Authorization` header's signature matches a freshly computed one.
fn verify_signature(input: VerifyInput<'_>) -> bool {
    let is_put = input.method == "PUT";
    let has_body = !input.body.is_empty();
    let content_length = if has_body {
        input.body.len().to_string()
    } else {
        String::new()
    };
    // Only a body-bearing PUT (block blob) sets content-type.
    let content_type = if is_put && has_body {
        "application/octet-stream"
    } else {
        ""
    };
    let mut x_ms_headers = vec![
        sharedkey::Header::new("x-ms-date", input.x_ms_date),
        sharedkey::Header::new("x-ms-version", input.x_ms_version),
    ];
    if !input.blob_type.is_empty() {
        x_ms_headers.push(sharedkey::Header::new("x-ms-blob-type", input.blob_type));
    }
    if !input.copy_source.is_empty() {
        x_ms_headers.push(sharedkey::Header::new(
            "x-ms-copy-source",
            input.copy_source,
        ));
    }
    // The mock is reached path-style (`/<container>/<blob>`); the canonical
    // resource always names the account. Query params (List Blobs) fold in as
    // sorted `\nname:value` lines.
    let mut canonical_resource = format!("/{ACCOUNT}{}", input.path);
    if !input.query.is_empty() {
        let mut params: Vec<(String, String)> = input
            .query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (k.to_ascii_lowercase(), urldecode(v)))
            .collect();
        params.sort();
        for (k, v) in params {
            canonical_resource.push('\n');
            canonical_resource.push_str(&k);
            canonical_resource.push(':');
            canonical_resource.push_str(&v);
        }
    }
    let req = sharedkey::Request {
        method: input.method,
        content_length: &content_length,
        content_type,
        x_ms_headers,
        canonical_resource: &canonical_resource,
    };
    match sharedkey::sign(&req, ACCOUNT, KEY) {
        Ok((expected_authz, _, _)) => expected_authz == input.authorization,
        Err(_) => false,
    }
}

/// Minimal percent-decoder for the query values the adapter sends (`%XX`).
fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
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

// -------------------------------------------------------------------------
// List Blobs / Copy Blob / Get Blob Properties — the operations added on top
// of the ContentStore put/get/delete contract, each a real Blob REST call.
// -------------------------------------------------------------------------

#[tokio::test]
async fn list_issues_signed_list_blobs_and_parses_names() {
    let (base, state) = spawn().await;
    *state.list_xml.lock().unwrap() = concat!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>",
        "<EnumerationResults><Blobs>",
        "<Blob><Name>docs/a/v1</Name></Blob>",
        "<Blob><Name>docs/b/v1</Name></Blob>",
        "</Blobs></EnumerationResults>"
    )
    .to_string();
    let store = store(&base);

    let names = store.list("docs/").await.unwrap();
    assert_eq!(
        names,
        vec!["docs/a/v1".to_string(), "docs/b/v1".to_string()]
    );

    let calls = state.calls.lock().unwrap();
    let rec = &calls[0];
    assert_eq!(rec.method, "GET");
    assert_eq!(rec.path, "/my-container");
    assert!(rec.query.contains("comp=list"), "query: {}", rec.query);
    assert!(
        rec.query.contains("restype=container"),
        "query: {}",
        rec.query
    );
    assert!(rec.query.contains("prefix=docs/"), "query: {}", rec.query);
    // The signature must verify over the query-bearing canonical resource.
    assert!(
        rec.signature_verified,
        "List Blobs signature did not verify: {}",
        rec.authorization
    );
}

#[tokio::test]
async fn list_without_prefix_lists_whole_container() {
    let (base, state) = spawn().await;
    *state.list_xml.lock().unwrap() =
        "<EnumerationResults><Blobs><Blob><Name>only</Name></Blob></Blobs></EnumerationResults>"
            .to_string();
    let store = store(&base);

    let names = store.list("").await.unwrap();
    assert_eq!(names, vec!["only".to_string()]);
    let calls = state.calls.lock().unwrap();
    assert!(
        !calls[0].query.contains("prefix="),
        "no prefix param: {}",
        calls[0].query
    );
    assert!(calls[0].signature_verified);
}

#[tokio::test]
async fn copy_sets_signed_copy_source_header() {
    let (base, state) = spawn().await;
    let store = store(&base);

    store.copy("docs/a/v1", "docs/a/v2").await.unwrap();

    let calls = state.calls.lock().unwrap();
    let rec = &calls[0];
    assert_eq!(rec.method, "PUT");
    assert_eq!(rec.path, "/my-container/docs/a/v2");
    // The copy source is the *full URL* of the source blob.
    assert!(
        rec.copy_source.ends_with("/my-container/docs/a/v1"),
        "copy source URL: {}",
        rec.copy_source
    );
    // Copy is body-less.
    assert!(rec.body.is_empty());
    assert_auth(rec);
}

#[tokio::test]
async fn copy_missing_source_maps_to_not_found() {
    let (base, state) = spawn().await;
    *state.not_found.lock().unwrap() = true;
    let store = store(&base);

    match store.copy("missing", "dst").await {
        Err(EcmError::NotFound) => {}
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[tokio::test]
async fn properties_parses_blob_metadata() {
    let (base, state) = spawn().await;
    let store = store(&base);

    let props = store.properties("docs/a/v1").await.unwrap();
    assert_eq!(props.content_length, 9);
    assert_eq!(props.content_type, "application/pdf");
    assert_eq!(props.etag, "\"0x8DA\"");

    let calls = state.calls.lock().unwrap();
    assert_eq!(calls[0].method, "HEAD");
    assert_eq!(calls[0].path, "/my-container/docs/a/v1");
    assert_auth(&calls[0]);
}

#[tokio::test]
async fn properties_missing_maps_to_not_found() {
    let (base, state) = spawn().await;
    *state.not_found.lock().unwrap() = true;
    let store = store(&base);

    match store.properties("gone").await {
        Err(EcmError::NotFound) => {}
        other => panic!("expected NotFound, got {other:?}"),
    }
}
