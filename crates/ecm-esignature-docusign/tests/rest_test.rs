//! Behavior tests for [`RestProvider`], ported from pyfly's
//! `tests/ecm/test_docusign_behavior.py`.
//!
//! Each test spins up an in-process axum mock on port 0 (the `httptest`/fake
//! pooled-client analog) and asserts BOTH the outbound request the adapter
//! builds (method, path, auth header, JSON payload) AND that the canned
//! response is parsed into the correct [`SignatureStatus`] / envelope id —
//! with no network, Docker, or real DocuSign involved.

use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use firefly_ecm::{ESignatureProvider, SignatureRequest, SignatureStatus};
use firefly_ecm_esignature_docusign::{map_status, RestProvider};

const ACCOUNT_ID: &str = "acct-123";
const ACCESS_TOKEN: &str = "tok-abc";

/// What the mock captured about the last request it served.
#[derive(Default, Clone)]
struct Captured {
    method: String,
    path: String,
    authorization: String,
    body: Value,
}

type Shared = Arc<Mutex<Captured>>;

/// Spawns an axum app on a random localhost port; returns its base URL.
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

// ---------------------------------------------------------------------------
// create()
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_builds_request_and_parses_envelope_id() {
    let shared: Shared = Arc::default();
    let app = Router::new()
        .route(
            "/v2.1/accounts/:acct/envelopes",
            post(
                |State(s): State<Shared>,
                 Path(acct): Path<String>,
                 headers: HeaderMap,
                 Json(body): Json<Value>| async move {
                    capture(
                        &s,
                        "POST",
                        format!("/v2.1/accounts/{acct}/envelopes"),
                        &headers,
                        body,
                    );
                    (
                        StatusCode::CREATED,
                        Json(json!({ "envelopeId": "env-789" })),
                    )
                },
            ),
        )
        .with_state(shared.clone());
    let base = spawn(app).await;

    let provider = RestProvider::new(format!("{base}/"), ACCOUNT_ID, ACCESS_TOKEN);
    let id = provider
        .create(SignatureRequest {
            document_id: "doc-1".into(),
            signers: vec!["alice@example.com".into(), "bob@example.com".into()],
            title: "Sign please".into(),
            provider: "docusign".into(),
        })
        .await
        .unwrap();

    // (b) parsed domain return
    assert_eq!(id, "env-789");

    // (a) outbound request
    let c = shared.lock().unwrap().clone();
    assert_eq!(c.method, "POST");
    assert_eq!(c.path, format!("/v2.1/accounts/{ACCOUNT_ID}/envelopes"));
    assert_eq!(c.authorization, format!("Bearer {ACCESS_TOKEN}"));
    assert_eq!(c.body["emailSubject"], "Sign please");
    assert_eq!(c.body["status"], "sent");
    assert_eq!(c.body["documents"][0]["documentId"], "doc-1");
    assert_eq!(c.body["documents"][0]["fileExtension"], "pdf");
    let signers = c.body["recipients"]["signers"].as_array().unwrap();
    assert_eq!(signers.len(), 2);
    assert_eq!(signers[0]["email"], "alice@example.com");
    assert_eq!(signers[1]["email"], "bob@example.com");
    // recipientId / routingOrder are 1-based strings.
    assert_eq!(signers[0]["recipientId"], "1");
    assert_eq!(signers[1]["recipientId"], "2");
    assert_eq!(signers[0]["routingOrder"], "1");
    assert_eq!(signers[1]["routingOrder"], "2");
}

#[tokio::test]
async fn create_errors_on_non_2xx() {
    let app = Router::new().route(
        "/v2.1/accounts/:acct/envelopes",
        post(|| async {
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "errorCode": "AUTH" })),
            )
        }),
    );
    let base = spawn(app).await;
    let provider = RestProvider::new(base, ACCOUNT_ID, ACCESS_TOKEN);

    let err = provider
        .create(SignatureRequest {
            document_id: "doc-err".into(),
            signers: vec!["carol@example.com".into()],
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "docusign: HTTP 401");
}

// ---------------------------------------------------------------------------
// status()  (pyfly's get())
// ---------------------------------------------------------------------------

#[tokio::test]
async fn status_maps_completed_to_signed() {
    let shared: Shared = Arc::default();
    let app = Router::new()
        .route(
            "/v2.1/accounts/:acct/envelopes/:id",
            get(
                |State(s): State<Shared>,
                 Path((acct, id)): Path<(String, String)>,
                 headers: HeaderMap| async move {
                    capture(
                        &s,
                        "GET",
                        format!("/v2.1/accounts/{acct}/envelopes/{id}"),
                        &headers,
                        Value::Null,
                    );
                    Json(json!({
                        "status": "completed",
                        "sentDateTime": "2026-06-01T10:00:00Z",
                        "completedDateTime": "2026-06-02T12:30:00Z",
                    }))
                },
            ),
        )
        .with_state(shared.clone());
    let base = spawn(app).await;
    let provider = RestProvider::new(base, ACCOUNT_ID, ACCESS_TOKEN);

    let status = provider.status("env-555").await.unwrap();
    assert_eq!(status, SignatureStatus::Signed);

    let c = shared.lock().unwrap().clone();
    assert_eq!(c.method, "GET");
    assert_eq!(
        c.path,
        format!("/v2.1/accounts/{ACCOUNT_ID}/envelopes/env-555")
    );
    assert_eq!(c.authorization, format!("Bearer {ACCESS_TOKEN}"));
}

#[tokio::test]
async fn status_returns_not_found_on_404() {
    let app = Router::new().route(
        "/v2.1/accounts/:acct/envelopes/:id",
        get(|| async { (StatusCode::NOT_FOUND, Json(json!({}))) }),
    );
    let base = spawn(app).await;
    let provider = RestProvider::new(base, ACCOUNT_ID, ACCESS_TOKEN);

    let err = provider.status("missing").await.unwrap_err();
    assert!(err.is_not_found());
}

// ---------------------------------------------------------------------------
// cancel()
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cancel_voids_envelope() {
    let shared: Shared = Arc::default();
    let app = Router::new()
        .route(
            "/v2.1/accounts/:acct/envelopes/:id",
            axum::routing::put(
                |State(s): State<Shared>,
                 Path((acct, id)): Path<(String, String)>,
                 headers: HeaderMap,
                 Json(body): Json<Value>| async move {
                    capture(
                        &s,
                        "PUT",
                        format!("/v2.1/accounts/{acct}/envelopes/{id}"),
                        &headers,
                        body,
                    );
                    (StatusCode::OK, Json(json!({})))
                },
            ),
        )
        .with_state(shared.clone());
    let base = spawn(app).await;
    let provider = RestProvider::new(base, ACCOUNT_ID, ACCESS_TOKEN);

    provider.cancel("env-321").await.unwrap();

    let c = shared.lock().unwrap().clone();
    assert_eq!(c.method, "PUT");
    assert_eq!(
        c.path,
        format!("/v2.1/accounts/{ACCOUNT_ID}/envelopes/env-321")
    );
    assert_eq!(c.body["status"], "voided");
    assert_eq!(c.body["voidedReason"], "cancelled by application");
    assert_eq!(c.authorization, format!("Bearer {ACCESS_TOKEN}"));
}

#[tokio::test]
async fn cancel_errors_on_non_2xx() {
    let app = Router::new().route(
        "/v2.1/accounts/:acct/envelopes/:id",
        axum::routing::put(|| async { (StatusCode::CONFLICT, Json(json!({}))) }),
    );
    let base = spawn(app).await;
    let provider = RestProvider::new(base, ACCOUNT_ID, ACCESS_TOKEN);

    let err = provider.cancel("env-409").await.unwrap_err();
    assert_eq!(err.to_string(), "docusign: HTTP 409");
}

// ---------------------------------------------------------------------------
// status-mapping table (pyfly's _map_status)
// ---------------------------------------------------------------------------

#[test]
fn status_mapping_table_matches_pyfly() {
    assert_eq!(map_status("created"), SignatureStatus::Pending);
    assert_eq!(map_status("sent"), SignatureStatus::Pending);
    assert_eq!(map_status("delivered"), SignatureStatus::Pending);
    assert_eq!(map_status("completed"), SignatureStatus::Signed);
    assert_eq!(map_status("declined"), SignatureStatus::Declined);
    assert_eq!(map_status("voided"), SignatureStatus::Declined);
    assert_eq!(map_status("expired"), SignatureStatus::Expired);
    // case-insensitive + unknown fallback
    assert_eq!(map_status("COMPLETED"), SignatureStatus::Signed);
    assert_eq!(map_status("mystery"), SignatureStatus::Pending);
}

#[test]
fn rest_provider_usable_as_trait_object() {
    let _p: Box<dyn ESignatureProvider> = Box::new(RestProvider::new("http://x", "a", "t"));
}
