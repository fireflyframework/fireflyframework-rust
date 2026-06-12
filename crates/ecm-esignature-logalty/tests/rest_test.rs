//! Behavior tests for [`RestProvider`], ported from pyfly's
//! `tests/ecm/test_logalty_behavior.py`.
//!
//! Each test spins up an in-process axum mock on port 0 and asserts BOTH the
//! outbound request the adapter builds (method, path, `X-Api-Key` header, JSON
//! payload) AND how it parses each response into [`SignatureStatus`] / the
//! envelope id — no network, no Docker.

use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use firefly_ecm::{ESignatureProvider, SignatureRequest, SignatureStatus};
use firefly_ecm_esignature_logalty::{map_status, RestProvider};

const API_KEY: &str = "secret-key-123";

#[derive(Default, Clone)]
struct Captured {
    method: String,
    path: String,
    api_key: String,
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
    c.api_key = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    c.body = body;
}

// ---------------------------------------------------------------------------
// create()  (pyfly send())
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_builds_request_and_parses_envelope_id() {
    let shared: Shared = Arc::default();
    let app = Router::new()
        .route(
            "/envelopes",
            post(
                |State(s): State<Shared>, headers: HeaderMap, Json(body): Json<Value>| async move {
                    capture(&s, "POST", "/envelopes".into(), &headers, body);
                    (
                        StatusCode::CREATED,
                        Json(json!({ "envelopeId": "env-789" })),
                    )
                },
            ),
        )
        .with_state(shared.clone());
    let base = spawn(app).await;
    let provider = RestProvider::new(format!("{base}/"), API_KEY);

    let id = provider
        .create(SignatureRequest {
            document_id: "doc-42".into(),
            signers: vec!["alice@example.com".into(), "bob@example.com".into()],
            title: "Sign this".into(),
            provider: "logalty".into(),
        })
        .await
        .unwrap();
    assert_eq!(id, "env-789");

    let c = shared.lock().unwrap().clone();
    assert_eq!(c.method, "POST");
    assert_eq!(c.path, "/envelopes");
    assert_eq!(c.api_key, API_KEY);
    assert_eq!(c.body["documentId"], "doc-42");
    assert_eq!(c.body["subject"], "Sign this");
    assert_eq!(
        c.body["signers"],
        json!([
            { "name": "alice@example.com", "email": "alice@example.com", "role": "signer" },
            { "name": "bob@example.com", "email": "bob@example.com", "role": "signer" },
        ])
    );
}

#[tokio::test]
async fn create_errors_on_non_2xx() {
    let app = Router::new().route(
        "/envelopes",
        post(|| async { (StatusCode::UNPROCESSABLE_ENTITY, "bad request") }),
    );
    let base = spawn(app).await;
    let provider = RestProvider::new(base, API_KEY);

    let err = provider
        .create(SignatureRequest {
            document_id: "doc-1".into(),
            signers: vec!["alice@example.com".into()],
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "logalty: HTTP 422");
}

// ---------------------------------------------------------------------------
// status()  (pyfly get())
// ---------------------------------------------------------------------------

#[tokio::test]
async fn status_maps_completed_to_signed() {
    let shared: Shared = Arc::default();
    let app = Router::new()
        .route(
            "/envelopes/:id",
            get(
                |State(s): State<Shared>, Path(id): Path<String>, headers: HeaderMap| async move {
                    capture(&s, "GET", format!("/envelopes/{id}"), &headers, Value::Null);
                    Json(json!({ "status": "COMPLETED" }))
                },
            ),
        )
        .with_state(shared.clone());
    let base = spawn(app).await;
    let provider = RestProvider::new(base, API_KEY);

    let status = provider.status("env-789").await.unwrap();
    assert_eq!(status, SignatureStatus::Signed);

    let c = shared.lock().unwrap().clone();
    assert_eq!(c.method, "GET");
    assert_eq!(c.path, "/envelopes/env-789");
    assert_eq!(c.api_key, API_KEY);
}

#[tokio::test]
async fn status_returns_not_found_on_404() {
    let app = Router::new().route("/envelopes/:id", get(|| async { StatusCode::NOT_FOUND }));
    let base = spawn(app).await;
    let provider = RestProvider::new(base, API_KEY);

    let err = provider.status("missing").await.unwrap_err();
    assert!(err.is_not_found());
}

// ---------------------------------------------------------------------------
// cancel()
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cancel_deletes_envelope_on_204() {
    let shared: Shared = Arc::default();
    let app = Router::new()
        .route(
            "/envelopes/:id",
            delete(
                |State(s): State<Shared>, Path(id): Path<String>, headers: HeaderMap| async move {
                    capture(
                        &s,
                        "DELETE",
                        format!("/envelopes/{id}"),
                        &headers,
                        Value::Null,
                    );
                    StatusCode::NO_CONTENT
                },
            ),
        )
        .with_state(shared.clone());
    let base = spawn(app).await;
    let provider = RestProvider::new(base, API_KEY);

    provider.cancel("env-1").await.unwrap();

    let c = shared.lock().unwrap().clone();
    assert_eq!(c.method, "DELETE");
    assert_eq!(c.path, "/envelopes/env-1");
    assert_eq!(c.api_key, API_KEY);
}

#[tokio::test]
async fn cancel_errors_on_non_2xx() {
    let app = Router::new().route("/envelopes/:id", delete(|| async { StatusCode::CONFLICT }));
    let base = spawn(app).await;
    let provider = RestProvider::new(base, API_KEY);

    let err = provider.cancel("env-2").await.unwrap_err();
    assert_eq!(err.to_string(), "logalty: HTTP 409");
}

// ---------------------------------------------------------------------------
// status-mapping table
// ---------------------------------------------------------------------------

#[test]
fn status_mapping_table_matches_pyfly() {
    assert_eq!(map_status("DRAFT"), SignatureStatus::Pending);
    assert_eq!(map_status("SENT"), SignatureStatus::Pending);
    assert_eq!(map_status("PENDING"), SignatureStatus::Pending);
    assert_eq!(map_status("SIGNED"), SignatureStatus::Signed);
    assert_eq!(map_status("COMPLETED"), SignatureStatus::Signed);
    assert_eq!(map_status("DECLINED"), SignatureStatus::Declined);
    assert_eq!(map_status("EXPIRED"), SignatureStatus::Expired);
    // case-insensitive + unknown fallback
    assert_eq!(map_status("completed"), SignatureStatus::Signed);
    assert_eq!(map_status("mystery"), SignatureStatus::Pending);
}

#[test]
fn rest_provider_usable_as_trait_object() {
    let _p: Box<dyn ESignatureProvider> = Box::new(RestProvider::new("http://x", "k"));
}
