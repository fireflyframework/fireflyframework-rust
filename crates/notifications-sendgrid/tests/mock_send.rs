//! Behavior tests for [`SendGridEmailProvider`] against an in-process axum
//! mock that asserts the exact outbound JSON — the Rust port of
//! pyfly `tests/notifications/test_sendgrid_behavior.py`.
//!
//! No network: the mock binds `127.0.0.1:0`, the provider is pointed at it via
//! `with_api_base`, and the handler captures the request body/headers so the
//! test can assert the SendGrid v3 `/mail/send` payload shape.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::Router;
use firefly_notifications::{Channel as _, Kind, Notification};
use firefly_notifications_sendgrid::{
    Attachment, Channel, Config, EmailMessage, EmailProvider, SendGridEmailProvider,
};
use serde_json::Value;

/// What the mock captured plus what it should answer with.
#[derive(Clone, Default)]
struct MockState {
    captured: Arc<Mutex<Vec<Captured>>>,
    status: StatusCode,
    message_id: Option<String>,
    body: String,
}

#[derive(Clone)]
struct Captured {
    path: String,
    authorization: Option<String>,
    content_type: Option<String>,
    json: Value,
}

async fn mail_send(
    State(state): State<MockState>,
    headers: HeaderMap,
    body: String,
) -> (StatusCode, HeaderMap, String) {
    let json: Value = serde_json::from_str(&body).expect("valid JSON body");
    state.captured.lock().unwrap().push(Captured {
        path: "/mail/send".to_string(),
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
    let mut out = HeaderMap::new();
    if let Some(id) = &state.message_id {
        out.insert("X-Message-Id", id.parse().unwrap());
    }
    (state.status, out, state.body.clone())
}

/// Spawns the mock, returning (base url, shared captured-requests vec).
async fn spawn_mock(
    status: StatusCode,
    message_id: Option<&str>,
    body: &str,
) -> (String, Arc<Mutex<Vec<Captured>>>) {
    let captured: Arc<Mutex<Vec<Captured>>> = Arc::new(Mutex::new(Vec::new()));
    let state = MockState {
        captured: captured.clone(),
        status,
        message_id: message_id.map(|s| s.to_string()),
        body: body.to_string(),
    };
    let app = Router::new()
        .route("/mail/send", post(mail_send))
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
    let (base, captured) = spawn_mock(StatusCode::ACCEPTED, Some("sg_msg_abc123"), "").await;
    let provider = SendGridEmailProvider::with_api_base("SG.test_key", base);

    let msg = EmailMessage {
        to: vec!["dest@example.com".into()],
        sender: "from@example.com".into(),
        subject: "Hello SendGrid".into(),
        body_text: Some("plain body".into()),
        body_html: Some("<p>html body</p>".into()),
        ..EmailMessage::default()
    };
    let result = EmailProvider::send(&provider, msg.clone()).await;

    let calls = captured.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    assert_eq!(call.path, "/mail/send");
    assert_eq!(call.authorization.as_deref(), Some("Bearer SG.test_key"));
    assert_eq!(call.content_type.as_deref(), Some("application/json"));

    let payload = &call.json;
    assert_eq!(
        payload["personalizations"][0]["to"],
        serde_json::json!([{ "email": "dest@example.com" }])
    );
    assert_eq!(payload["personalizations"][0]["subject"], "Hello SendGrid");
    assert_eq!(payload["from"]["email"], "from@example.com");

    // content: both text and html, in that order
    let content = payload["content"].as_array().unwrap();
    let by_type: std::collections::HashMap<&str, &str> = content
        .iter()
        .map(|c| (c["type"].as_str().unwrap(), c["value"].as_str().unwrap()))
        .collect();
    assert_eq!(by_type["text/plain"], "plain body");
    assert_eq!(by_type["text/html"], "<p>html body</p>");

    // empty cc/bcc absent
    assert!(payload["personalizations"][0].get("cc").is_none());
    assert!(payload["personalizations"][0].get("bcc").is_none());

    assert_eq!(result.status.as_str(), "SENT");
    assert_eq!(result.provider, "sendgrid");
    assert_eq!(result.id, msg.id);
    assert_eq!(result.provider_id.as_deref(), Some("sg_msg_abc123"));
    assert!(result.error.is_none());
}

#[tokio::test]
async fn send_includes_cc_bcc_and_base64_attachments() {
    use base64::Engine as _;
    let raw = b"hello-bytes".to_vec();
    let (base, captured) = spawn_mock(StatusCode::ACCEPTED, Some("sg_xyz"), "").await;
    let provider = SendGridEmailProvider::with_api_base("SG.key", base);

    let msg = EmailMessage {
        to: vec!["a@x.io".into()],
        cc: vec!["c@x.io".into()],
        bcc: vec!["b@x.io".into()],
        sender: "s@x.io".into(),
        subject: "rich".into(),
        body_html: Some("<p>hi</p>".into()),
        attachments: vec![Attachment::new("f.txt", "text/plain", raw.clone())],
        ..EmailMessage::default()
    };
    let result = EmailProvider::send(&provider, msg).await;

    let calls = captured.lock().unwrap();
    let payload = &calls[0].json;
    assert_eq!(
        payload["personalizations"][0]["cc"],
        serde_json::json!([{ "email": "c@x.io" }])
    );
    assert_eq!(
        payload["personalizations"][0]["bcc"],
        serde_json::json!([{ "email": "b@x.io" }])
    );
    let expected_b64 = base64::engine::general_purpose::STANDARD.encode(&raw);
    assert_eq!(
        payload["attachments"],
        serde_json::json!([{
            "filename": "f.txt",
            "type": "text/plain",
            "content": expected_b64,
        }])
    );
    assert_eq!(result.status.as_str(), "SENT");
    assert_eq!(result.provider_id.as_deref(), Some("sg_xyz"));
}

#[tokio::test]
async fn send_template_id_sets_dynamic_template_data() {
    let (base, captured) = spawn_mock(StatusCode::ACCEPTED, Some("sg_tmpl"), "").await;
    let provider = SendGridEmailProvider::with_api_base("SG.key", base);

    let mut template_data = std::collections::BTreeMap::new();
    template_data.insert("name".to_string(), serde_json::json!("Alice"));
    let msg = EmailMessage {
        to: vec!["u@x.io".into()],
        sender: "s@x.io".into(),
        subject: "tmpl".into(),
        template_id: Some("d-abc123".into()),
        template_data,
        ..EmailMessage::default()
    };
    let result = EmailProvider::send(&provider, msg).await;

    let calls = captured.lock().unwrap();
    let payload = &calls[0].json;
    assert_eq!(payload["template_id"], "d-abc123");
    assert_eq!(
        payload["personalizations"][0]["dynamic_template_data"],
        serde_json::json!({ "name": "Alice" })
    );
    assert_eq!(result.status.as_str(), "SENT");
}

#[tokio::test]
async fn send_maps_non_2xx_to_failed_result() {
    let (base, _captured) = spawn_mock(StatusCode::BAD_REQUEST, None, "bad request").await;
    let provider = SendGridEmailProvider::with_api_base("SG.key", base);

    let msg = EmailMessage {
        to: vec!["bad@x.io".into()],
        sender: "from@x.io".into(),
        subject: "oops".into(),
        ..EmailMessage::default()
    };
    let result = EmailProvider::send(&provider, msg.clone()).await;

    assert_eq!(result.status.as_str(), "FAILED");
    assert_eq!(result.provider, "sendgrid");
    assert_eq!(result.id, msg.id);
    assert!(result.provider_id.is_none());
    let err = result.error.unwrap();
    assert!(err.contains("400"), "{err}");
    assert!(err.contains("bad request"), "{err}");
}

#[tokio::test]
async fn send_transport_error_maps_to_failed() {
    // Point at a closed port — connection refused, no panic, FAILED result.
    let provider = SendGridEmailProvider::with_api_base("SG.key", "http://127.0.0.1:1");
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

// --- Go-parity Channel adapter: real /mail/send through the envelope -------

#[tokio::test]
async fn channel_send_maps_envelope_and_posts_real_request() {
    let (base, captured) = spawn_mock(StatusCode::ACCEPTED, Some("sg_chan_1"), "").await;
    let channel = Channel::with_api_base(
        Config {
            api_key: "SG.chan_key".into(),
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
    assert_eq!(call.path, "/mail/send");
    assert_eq!(call.authorization.as_deref(), Some("Bearer SG.chan_key"));
    let payload = &call.json;
    // from_address from the Config is used as the sender.
    assert_eq!(payload["from"]["email"], "noreply@firefly.io");
    assert_eq!(
        payload["personalizations"][0]["to"],
        serde_json::json!([{ "email": "alice@example.com" }])
    );
    assert_eq!(payload["personalizations"][0]["subject"], "Welcome");
    // body maps to a text/plain content part.
    let content = payload["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "text/plain");
    assert_eq!(content[0]["value"], "Welcome to Firefly!");
}

#[tokio::test]
async fn channel_send_non_2xx_maps_to_delivery_error() {
    let (base, _captured) = spawn_mock(StatusCode::BAD_REQUEST, None, "invalid sender").await;
    let channel = Channel::with_api_base(
        Config {
            api_key: "SG.key".into(),
            from_address: "noreply@firefly.io".into(),
            ..Config::default()
        },
        base,
    );

    let err = channel
        .send(Notification {
            channel: Kind::EMAIL,
            to: "alice@example.com".into(),
            subject: "x".into(),
            body: "y".into(),
            ..Notification::default()
        })
        .await
        .expect_err("a 400 from SendGrid must surface as a Delivery error");
    let msg = err.to_string();
    assert!(msg.contains("400"), "{msg}");
    assert!(msg.contains("invalid sender"), "{msg}");
}
