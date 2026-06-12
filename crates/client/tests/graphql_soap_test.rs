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

//! Integration tests for the GraphQL and SOAP protocol clients, ported
//! from pyfly's `test_graphql_client_transport.py` and
//! `test_protocols.py`. The respx HTTP mocks become an in-process axum
//! server on a random localhost port — the same pattern the REST suite
//! uses.

use std::sync::{Arc, Mutex};

use axum::extract::Json;
use axum::http::{header, HeaderMap, StatusCode};
use axum::routing::post;
use axum::Router;
use serde::Deserialize;
use serde_json::{json, Value};

use firefly_client::{
    no_variables, wrap_envelope, GraphQlBuilder, GraphQlClient, SoapBuilder, SoapClient,
};

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

// ===========================================================================
// GraphQL
// ===========================================================================

// --- pyfly: test_graphql_execute_posts_correct_envelope -------------------

#[tokio::test]
async fn graphql_execute_posts_correct_envelope() {
    let captured: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let seen = captured.clone();
    let app = Router::new().route(
        "/graphql",
        post(move |Json(body): Json<Value>| {
            let seen = seen.clone();
            async move {
                *seen.lock().expect("lock") = Some(body);
                Json(json!({ "data": { "user": { "id": "1" } } }))
            }
        }),
    );
    let base = spawn_server(app).await;

    let client = GraphQlBuilder::new(format!("{base}/graphql")).build();

    #[derive(Deserialize)]
    struct Data {
        user: User,
    }
    #[derive(Deserialize)]
    struct User {
        id: String,
    }

    let data: Data = client
        .execute(
            "{ user { id } }",
            Some(&json!({ "userId": "1" })),
            Some("GetUser"),
        )
        .await
        .expect("execute");
    assert_eq!(data.user.id, "1");

    let body = captured.lock().expect("lock").take().expect("body");
    assert_eq!(body["query"], json!("{ user { id } }"));
    assert_eq!(body["variables"], json!({ "userId": "1" }));
    assert_eq!(body["operationName"], json!("GetUser"));
}

// --- pyfly: test_graphql_execute_omits_none_fields ------------------------

#[tokio::test]
async fn graphql_execute_omits_none_fields() {
    let captured: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let seen = captured.clone();
    let app = Router::new().route(
        "/graphql",
        post(move |Json(body): Json<Value>| {
            let seen = seen.clone();
            async move {
                *seen.lock().expect("lock") = Some(body);
                Json(json!({ "data": { "ping": true } }))
            }
        }),
    );
    let base = spawn_server(app).await;

    let client = GraphQlClient::builder(format!("{base}/graphql")).build();
    let _: Value = client
        .execute("{ ping }", no_variables(), None)
        .await
        .expect("execute");

    let body = captured.lock().expect("lock").take().expect("body");
    let obj = body.as_object().expect("object");
    assert!(!obj.contains_key("variables"), "variables must be omitted");
    assert!(
        !obj.contains_key("operationName"),
        "operationName must be omitted"
    );
}

// --- pyfly: test_graphql_execute_raises_on_errors -------------------------

#[tokio::test]
async fn graphql_execute_raises_on_errors() {
    let app = Router::new().route(
        "/graphql",
        post(|| async { Json(json!({ "errors": [{ "message": "Not found" }] })) }),
    );
    let base = spawn_server(app).await;

    let client = GraphQlBuilder::new(format!("{base}/graphql")).build();
    let err = client
        .execute::<Value, Value>("{ missing }", no_variables(), None)
        .await
        .expect_err("expected GraphQL errors");
    match err {
        firefly_client::ClientError::GraphQl(errors) => {
            assert_eq!(errors.len(), 1);
            assert_eq!(errors[0]["message"], json!("Not found"));
        }
        other => panic!("expected ClientError::GraphQl, got {other:?}"),
    }
}

#[tokio::test]
async fn graphql_empty_errors_array_is_not_an_error() {
    // A spec-compliant response may carry an empty `errors` array; that
    // is success, not failure.
    let app = Router::new().route(
        "/graphql",
        post(|| async { Json(json!({ "data": { "ok": true }, "errors": [] })) }),
    );
    let base = spawn_server(app).await;

    let client = GraphQlBuilder::new(format!("{base}/graphql")).build();
    let data: Value = client
        .execute("{ ok }", no_variables(), None)
        .await
        .expect("empty errors is success");
    assert_eq!(data, json!({ "ok": true }));
}

#[tokio::test]
async fn graphql_non_2xx_surfaces_as_problem() {
    let app = Router::new().route(
        "/graphql",
        post(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "boom") }),
    );
    let base = spawn_server(app).await;

    let client = GraphQlBuilder::new(format!("{base}/graphql")).build();
    let err = client
        .execute::<Value, Value>("{ x }", no_variables(), None)
        .await
        .expect_err("expected problem");
    assert_eq!(err.status(), Some(500));
}

#[tokio::test]
async fn graphql_forwards_default_headers() {
    let captured: Arc<Mutex<Option<HeaderMap>>> = Arc::new(Mutex::new(None));
    let seen = captured.clone();
    let app = Router::new().route(
        "/graphql",
        post(move |headers: HeaderMap, _body: String| {
            let seen = seen.clone();
            async move {
                *seen.lock().expect("lock") = Some(headers);
                Json(json!({ "data": {} }))
            }
        }),
    );
    let base = spawn_server(app).await;

    let client = GraphQlBuilder::new(format!("{base}/graphql"))
        .with_header("Authorization", "Bearer token")
        .build();
    let _: Value = client
        .execute("{ x }", no_variables(), None)
        .await
        .expect("execute");

    let headers = captured.lock().expect("lock").take().expect("headers");
    assert_eq!(
        headers.get("authorization").and_then(|v| v.to_str().ok()),
        Some("Bearer token")
    );
    assert_eq!(
        headers.get("content-type").and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
}

// ===========================================================================
// SOAP
// ===========================================================================

// --- pyfly: test_soap_call_wraps_body_in_envelope ------------------------

#[tokio::test]
async fn soap_call_wraps_body_in_envelope() {
    let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let seen = captured.clone();
    let app = Router::new().route(
        "/service",
        post(move |body: String| {
            let seen = seen.clone();
            async move {
                *seen.lock().expect("lock") = Some(body);
                ([(header::CONTENT_TYPE, "text/xml")], "<ok/>")
            }
        }),
    );
    let base = spawn_server(app).await;

    let client = SoapBuilder::new(format!("{base}/service"))
        .with_action("DoThing")
        .build();
    let result = client
        .call("<GetFoo><id>42</id></GetFoo>")
        .await
        .expect("call");
    assert_eq!(result, "<ok/>");

    let body = captured.lock().expect("lock").take().expect("body");
    assert!(body.contains("soap:Envelope"), "envelope wrapper present");
    assert!(body.contains("soap:Body"), "body wrapper present");
    assert!(
        body.contains("<GetFoo><id>42</id></GetFoo>"),
        "payload embedded verbatim"
    );
}

// --- pyfly: test_soap_call_sends_soap_action_header ----------------------

#[tokio::test]
async fn soap_call_sends_soap_action_header() {
    let captured: Arc<Mutex<Option<HeaderMap>>> = Arc::new(Mutex::new(None));
    let seen = captured.clone();
    let app = Router::new().route(
        "/service",
        post(move |headers: HeaderMap, _body: String| {
            let seen = seen.clone();
            async move {
                *seen.lock().expect("lock") = Some(headers);
                "<resp/>"
            }
        }),
    );
    let base = spawn_server(app).await;

    let client = SoapBuilder::new(format!("{base}/service"))
        .with_action("MyAction")
        .build();
    client.call("<Payload/>").await.expect("call");

    let headers = captured.lock().expect("lock").take().expect("headers");
    assert_eq!(
        headers.get("soapaction").and_then(|v| v.to_str().ok()),
        Some("MyAction")
    );
    assert!(headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .contains("text/xml"));
}

#[tokio::test]
async fn soap_omits_action_header_when_unset() {
    let captured: Arc<Mutex<Option<HeaderMap>>> = Arc::new(Mutex::new(None));
    let seen = captured.clone();
    let app = Router::new().route(
        "/service",
        post(move |headers: HeaderMap, _body: String| {
            let seen = seen.clone();
            async move {
                *seen.lock().expect("lock") = Some(headers);
                "<resp/>"
            }
        }),
    );
    let base = spawn_server(app).await;

    let client = SoapClient::builder(format!("{base}/service")).build();
    client.call("<Payload/>").await.expect("call");

    let headers = captured.lock().expect("lock").take().expect("headers");
    assert!(
        headers.get("soapaction").is_none(),
        "no SOAPAction header without an action"
    );
}

#[tokio::test]
async fn soap_non_2xx_surfaces_as_problem() {
    let app = Router::new().route(
        "/service",
        post(|| async { (StatusCode::SERVICE_UNAVAILABLE, "<Fault/>") }),
    );
    let base = spawn_server(app).await;

    let client = SoapBuilder::new(format!("{base}/service")).build();
    let err = client
        .call("<Payload/>")
        .await
        .expect_err("expected problem");
    let fe = err.as_firefly().expect("FireflyError");
    assert_eq!(fe.status, 503);
    assert_eq!(fe.detail, "<Fault/>");
}

#[test]
fn wrap_envelope_matches_pyfly_template() {
    let env = wrap_envelope("<X/>");
    assert!(env.starts_with(r#"<?xml version="1.0" encoding="UTF-8"?>"#));
    assert!(
        env.contains(r#"<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">"#)
    );
    assert!(env.contains("<soap:Header/>"));
    assert!(env.contains("<soap:Body><X/></soap:Body>"));
    assert!(env.ends_with("</soap:Envelope>"));
}
