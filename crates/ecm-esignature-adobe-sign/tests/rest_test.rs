//! Behavior tests for [`RestProvider`], ported from pyfly's
//! `tests/ecm/test_adobe_sign_behavior.py`.
//!
//! Each test spins up an in-process axum mock on port 0 and asserts BOTH the
//! outbound request the adapter builds (method, path, auth header, JSON
//! payload) AND how it parses each response into [`SignatureStatus`] / the
//! agreement id — no network, no Docker.

use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde_json::{json, Value};

use firefly_ecm::{ESignatureProvider, SignatureRequest, SignatureStatus};
use firefly_ecm_esignature_adobe_sign::{map_status, RestProvider};

const TOKEN: &str = "secret-integration-key";

#[derive(Default, Clone)]
struct Captured {
    method: String,
    path: String,
    authorization: String,
    body: Value,
}

type Shared = Arc<Mutex<Captured>>;

async fn spawn(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

fn capture(shared: &Shared, method: &str, path: String, headers: &HeaderMap, body: Value) {
    let mut c = shared.lock().unwrap();
    c.method = method.to_string();
    c.path = path;
    c.authorization = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    c.body = body;
}

fn signature_request() -> SignatureRequest {
    SignatureRequest {
        document_id: "transient-doc-123".into(),
        signers: vec!["alice@example.com".into(), "bob@example.com".into()],
        title: "Loan agreement".into(),
        provider: "adobesign".into(),
    }
}

// ---------------------------------------------------------------------------
// create()  (pyfly send())
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_builds_request_and_parses_id() {
    let shared: Shared = Arc::default();
    let app = Router::new()
        .route(
            "/agreements",
            post(
                |State(s): State<Shared>, headers: HeaderMap, Json(body): Json<Value>| async move {
                    capture(&s, "POST", "/agreements".into(), &headers, body);
                    (
                        StatusCode::CREATED,
                        Json(json!({ "id": "CBJCHBCAABAA-agreement-id" })),
                    )
                },
            ),
        )
        .with_state(shared.clone());
    let base = spawn(app).await;
    let provider = RestProvider::new(format!("{base}/"), TOKEN);

    let id = provider.create(signature_request()).await.unwrap();
    assert_eq!(id, "CBJCHBCAABAA-agreement-id");

    let c = shared.lock().unwrap().clone();
    assert_eq!(c.method, "POST");
    assert_eq!(c.path, "/agreements");
    assert_eq!(c.authorization, format!("Bearer {TOKEN}"));
    assert_eq!(
        c.body["fileInfos"],
        json!([{ "transientDocumentId": "transient-doc-123" }])
    );
    assert_eq!(c.body["name"], "Loan agreement");
    assert_eq!(c.body["signatureType"], "ESIGN");
    assert_eq!(c.body["state"], "IN_PROCESS");
    assert_eq!(
        c.body["participantSetsInfo"],
        json!([
            { "memberInfos": [{ "email": "alice@example.com" }], "order": 1, "role": "SIGNER" },
            { "memberInfos": [{ "email": "bob@example.com" }], "order": 2, "role": "SIGNER" },
        ])
    );
}

#[tokio::test]
async fn create_errors_on_non_2xx() {
    let app = Router::new().route(
        "/agreements",
        post(|| async { (StatusCode::BAD_REQUEST, "INVALID_FILE_INFO") }),
    );
    let base = spawn(app).await;
    let provider = RestProvider::new(base, TOKEN);

    let err = provider.create(signature_request()).await.unwrap_err();
    assert_eq!(err.to_string(), "adobe-sign: HTTP 400");
}

// ---------------------------------------------------------------------------
// status()  (pyfly get())
// ---------------------------------------------------------------------------

#[tokio::test]
async fn status_maps_signed() {
    let shared: Shared = Arc::default();
    let app = Router::new()
        .route(
            "/agreements/:id",
            get(
                |State(s): State<Shared>, Path(id): Path<String>, headers: HeaderMap| async move {
                    capture(
                        &s,
                        "GET",
                        format!("/agreements/{id}"),
                        &headers,
                        Value::Null,
                    );
                    Json(json!({ "status": "SIGNED" }))
                },
            ),
        )
        .with_state(shared.clone());
    let base = spawn(app).await;
    let provider = RestProvider::new(base, TOKEN);

    let status = provider.status("agreement-42").await.unwrap();
    assert_eq!(status, SignatureStatus::Signed);

    let c = shared.lock().unwrap().clone();
    assert_eq!(c.method, "GET");
    assert_eq!(c.path, "/agreements/agreement-42");
    assert_eq!(c.authorization, format!("Bearer {TOKEN}"));
}

#[tokio::test]
async fn status_maps_out_for_signature_to_pending() {
    let app = Router::new().route(
        "/agreements/:id",
        get(|| async { Json(json!({ "status": "OUT_FOR_SIGNATURE" })) }),
    );
    let base = spawn(app).await;
    let provider = RestProvider::new(base, TOKEN);

    let status = provider.status("agreement-99").await.unwrap();
    assert_eq!(status, SignatureStatus::Pending);
}

#[tokio::test]
async fn status_returns_not_found_on_404() {
    let app = Router::new().route(
        "/agreements/:id",
        get(|| async { (StatusCode::NOT_FOUND, "not found") }),
    );
    let base = spawn(app).await;
    let provider = RestProvider::new(base, TOKEN);

    let err = provider.status("missing-agreement").await.unwrap_err();
    assert!(err.is_not_found());
}

// ---------------------------------------------------------------------------
// cancel()
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cancel_sends_cancel_state() {
    let shared: Shared = Arc::default();
    let app = Router::new()
        .route(
            "/agreements/:id/state",
            put(
                |State(s): State<Shared>,
                 Path(id): Path<String>,
                 headers: HeaderMap,
                 Json(body): Json<Value>| async move {
                    capture(&s, "PUT", format!("/agreements/{id}/state"), &headers, body);
                    (StatusCode::OK, Json(json!({})))
                },
            ),
        )
        .with_state(shared.clone());
    let base = spawn(app).await;
    let provider = RestProvider::new(base, TOKEN);

    provider.cancel("agreement-7").await.unwrap();

    let c = shared.lock().unwrap().clone();
    assert_eq!(c.method, "PUT");
    assert_eq!(c.path, "/agreements/agreement-7/state");
    assert_eq!(c.body["state"], "CANCELLED");
    assert_eq!(
        c.body["agreementCancellationInfo"]["comment"],
        "cancelled by app"
    );
    assert_eq!(c.authorization, format!("Bearer {TOKEN}"));
}

#[tokio::test]
async fn cancel_accepts_204() {
    let app = Router::new().route(
        "/agreements/:id/state",
        put(|| async { StatusCode::NO_CONTENT }),
    );
    let base = spawn(app).await;
    let provider = RestProvider::new(base, TOKEN);
    // 204 is a success (pyfly accepts 200/204).
    provider.cancel("agreement-204").await.unwrap();
}

#[tokio::test]
async fn cancel_errors_on_non_2xx() {
    let app = Router::new().route(
        "/agreements/:id/state",
        put(|| async { (StatusCode::FORBIDDEN, "forbidden") }),
    );
    let base = spawn(app).await;
    let provider = RestProvider::new(base, TOKEN);

    let err = provider.cancel("agreement-8").await.unwrap_err();
    assert_eq!(err.to_string(), "adobe-sign: HTTP 403");
}

// ---------------------------------------------------------------------------
// status-mapping table
// ---------------------------------------------------------------------------

#[test]
fn status_mapping_table_matches_pyfly() {
    assert_eq!(map_status("OUT_FOR_SIGNATURE"), SignatureStatus::Pending);
    assert_eq!(
        map_status("WAITING_FOR_MY_SIGNATURE"),
        SignatureStatus::Pending
    );
    assert_eq!(map_status("SIGNED"), SignatureStatus::Signed);
    assert_eq!(map_status("COMPLETED"), SignatureStatus::Signed);
    assert_eq!(map_status("CANCELLED"), SignatureStatus::Declined);
    assert_eq!(map_status("DECLINED"), SignatureStatus::Declined);
    assert_eq!(map_status("EXPIRED"), SignatureStatus::Expired);
    // case-insensitive + unknown fallback
    assert_eq!(map_status("signed"), SignatureStatus::Signed);
    assert_eq!(map_status("mystery"), SignatureStatus::Pending);
}

#[test]
fn rest_provider_usable_as_trait_object() {
    let _p: Box<dyn ESignatureProvider> = Box::new(RestProvider::new("http://x", "t"));
}
