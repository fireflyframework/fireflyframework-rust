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

//! Behavior tests for [`ResendEmailProvider`] against an in-process axum mock
//! that asserts the exact outbound JSON — the Rust port of
//! pyfly `tests/notifications/test_resend_behavior.py`.
//!
//! No network: the mock binds `127.0.0.1:0`, the provider is pointed at it via
//! `with_api_base`, and the handler captures the request body/headers so the
//! test can assert the Resend `/emails` payload shape.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::Router;
use firefly_notifications::{Channel as _, Kind, Notification};
use firefly_notifications_resend::{
    Attachment, Channel, Config, EmailMessage, EmailProvider, ResendEmailProvider,
};
use serde_json::Value;

#[derive(Clone)]
struct MockState {
    captured: Arc<Mutex<Vec<Captured>>>,
    status: StatusCode,
    response_json: String,
    error_body: String,
}

#[derive(Clone)]
struct Captured {
    path: String,
    authorization: Option<String>,
    content_type: Option<String>,
    json: Value,
}

async fn emails(
    State(state): State<MockState>,
    headers: HeaderMap,
    body: String,
) -> (StatusCode, String) {
    let json: Value = serde_json::from_str(&body).expect("valid JSON body");
    state.captured.lock().unwrap().push(Captured {
        path: "/emails".to_string(),
        authorization: headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string()),
        content_type: headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string()),
        json,
    });
    if state.status.is_success() {
        (state.status, state.response_json.clone())
    } else {
        (state.status, state.error_body.clone())
    }
}

async fn spawn_mock(
    status: StatusCode,
    response_json: &str,
    error_body: &str,
) -> (String, Arc<Mutex<Vec<Captured>>>) {
    let captured: Arc<Mutex<Vec<Captured>>> = Arc::new(Mutex::new(Vec::new()));
    let state = MockState {
        captured: captured.clone(),
        status,
        response_json: response_json.to_string(),
        error_body: error_body.to_string(),
    };
    let app = Router::new()
        .route("/emails", post(emails))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), captured)
}

#[tokio::test]
async fn send_builds_request_and_parses_sent_result() {
    let (base, captured) = spawn_mock(StatusCode::OK, r#"{"id":"re_abc123"}"#, "").await;
    let provider = ResendEmailProvider::new("re_test_key").with_api_base(base);

    let msg = EmailMessage {
        to: vec!["dest@example.com".into()],
        sender: "from@example.com".into(),
        subject: "Hello".into(),
        body_text: Some("plain body".into()),
        ..EmailMessage::default()
    };
    let result = EmailProvider::send(&provider, msg.clone()).await;

    let calls = captured.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    assert_eq!(call.path, "/emails");
    assert_eq!(call.authorization.as_deref(), Some("Bearer re_test_key"));
    assert_eq!(call.content_type.as_deref(), Some("application/json"));

    let payload = &call.json;
    assert_eq!(payload["from"], "from@example.com");
    assert_eq!(payload["to"], serde_json::json!(["dest@example.com"]));
    assert_eq!(payload["subject"], "Hello");
    assert_eq!(payload["text"], "plain body");
    assert!(payload.get("html").is_none());
    assert!(payload.get("cc").is_none());

    assert_eq!(result.status.as_str(), "SENT");
    assert_eq!(result.provider, "resend");
    assert_eq!(result.id, msg.id);
    assert_eq!(result.provider_id.as_deref(), Some("re_abc123"));
    assert!(result.error.is_none());
}

#[tokio::test]
async fn send_includes_cc_bcc_html_and_base64_attachments() {
    use base64::Engine as _;
    let raw = b"hello-bytes".to_vec();
    let (base, captured) = spawn_mock(StatusCode::ACCEPTED, r#"{"id":"re_xyz"}"#, "").await;
    let provider = ResendEmailProvider::new("re_key")
        .with_default_from("default@x.io")
        .with_api_base(base);

    let msg = EmailMessage {
        to: vec!["a@x.io".into()],
        cc: vec!["c@x.io".into()],
        bcc: vec!["b@x.io".into()],
        // sender empty -> default_from is used
        subject: "rich".into(),
        body_html: Some("<p>hi</p>".into()),
        attachments: vec![Attachment::new("f.txt", "text/plain", raw.clone())],
        ..EmailMessage::default()
    };
    let result = EmailProvider::send(&provider, msg).await;

    let calls = captured.lock().unwrap();
    let payload = &calls[0].json;
    assert_eq!(payload["from"], "default@x.io");
    assert_eq!(payload["cc"], serde_json::json!(["c@x.io"]));
    assert_eq!(payload["bcc"], serde_json::json!(["b@x.io"]));
    assert_eq!(payload["html"], "<p>hi</p>");
    assert!(payload.get("text").is_none());
    let expected_b64 = base64::engine::general_purpose::STANDARD.encode(&raw);
    assert_eq!(
        payload["attachments"],
        serde_json::json!([{ "filename": "f.txt", "content": expected_b64 }])
    );
    assert_eq!(result.status.as_str(), "SENT");
    assert_eq!(result.provider_id.as_deref(), Some("re_xyz"));
}

#[tokio::test]
async fn send_maps_non_2xx_to_failed_result() {
    let (base, _captured) =
        spawn_mock(StatusCode::UNPROCESSABLE_ENTITY, "", "invalid recipient").await;
    let provider = ResendEmailProvider::new("re_key").with_api_base(base);

    let msg = EmailMessage {
        to: vec!["bad@x.io".into()],
        sender: "from@x.io".into(),
        subject: "oops".into(),
        ..EmailMessage::default()
    };
    let result = EmailProvider::send(&provider, msg.clone()).await;

    assert_eq!(result.status.as_str(), "FAILED");
    assert_eq!(result.provider, "resend");
    assert_eq!(result.id, msg.id);
    assert!(result.provider_id.is_none());
    assert_eq!(result.error.as_deref(), Some("http 422: invalid recipient"));
}

#[tokio::test]
async fn send_transport_error_maps_to_failed() {
    let provider = ResendEmailProvider::new("re_key").with_api_base("http://127.0.0.1:1");
    let msg = EmailMessage {
        to: vec!["a@x.io".into()],
        sender: "s@x.io".into(),
        subject: "x".into(),
        body_text: Some("b".into()),
        ..EmailMessage::default()
    };
    let result = EmailProvider::send(&provider, msg).await;
    assert_eq!(result.status.as_str(), "FAILED");
    assert!(result.error.is_some());
    assert!(result.provider_id.is_none());
}

// --- Go-parity Channel adapter: real /emails through the envelope ----------

#[tokio::test]
async fn channel_send_maps_envelope_and_posts_real_request() {
    let (base, captured) = spawn_mock(StatusCode::OK, r#"{"id":"re_chan_1"}"#, "").await;
    let channel = Channel::with_api_base(
        Config {
            api_key: "re_chan_key".into(),
            from_address: "noreply@firefly.io".into(),
            ..Config::default()
        },
        base,
    );

    channel
        .send(Notification {
            channel: Kind::EMAIL,
            to: "alice@example.com".into(),
            subject: "Welcome".into(),
            body: "Welcome to Firefly!".into(),
            ..Notification::default()
        })
        .await
        .expect("channel send should reach the mock and succeed");

    let calls = captured.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    assert_eq!(call.path, "/emails");
    assert_eq!(call.authorization.as_deref(), Some("Bearer re_chan_key"));
    let payload = &call.json;
    // from_address from the Config is used as the sender (default_from).
    assert_eq!(payload["from"], "noreply@firefly.io");
    assert_eq!(payload["to"], serde_json::json!(["alice@example.com"]));
    assert_eq!(payload["subject"], "Welcome");
    assert_eq!(payload["text"], "Welcome to Firefly!");
}

#[tokio::test]
async fn channel_send_non_2xx_maps_to_delivery_error() {
    let (base, _captured) =
        spawn_mock(StatusCode::UNPROCESSABLE_ENTITY, "", "invalid recipient").await;
    let channel = Channel::with_api_base(
        Config {
            api_key: "re_key".into(),
            from_address: "noreply@firefly.io".into(),
            ..Config::default()
        },
        base,
    );

    let err = channel
        .send(Notification {
            channel: Kind::EMAIL,
            to: "bad@example.com".into(),
            subject: "x".into(),
            body: "y".into(),
            ..Notification::default()
        })
        .await
        .expect_err("a 422 from Resend must surface as a Delivery error");
    let msg = err.to_string();
    assert!(msg.contains("422"), "{msg}");
    assert!(msg.contains("invalid recipient"), "{msg}");
}
