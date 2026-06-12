//! Behavior tests for [`TwilioSmsProvider`], ported 1:1 from pyfly's
//! `tests/notifications/test_twilio_behavior.py`.
//!
//! Where pyfly injects a fake `httpx` client and asserts on the recorded call
//! kwargs, this Rust port spins up an in-process axum mock on `127.0.0.1:0` and
//! asserts on the *actual* request bytes the adapter put on the wire: the URL,
//! the HTTP basic-auth header, and the form-encoded `From`/`To`/`Body` fields.
//! No network, no Docker; every test completes in milliseconds.

use std::sync::{Arc, Mutex};

use axum::extract::{Form, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::Router;
use serde::Deserialize;

use firefly_notifications_twilio::{
    DeliveryStatus, NotificationResult, SmsMessage, SmsProvider, TwilioError, TwilioSmsProvider,
};

/// One recorded inbound request to the mock `Messages.json` endpoint.
#[derive(Clone, Debug)]
struct Recorded {
    account_sid: String,
    authorization: Option<String>,
    form: TwilioForm,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
struct TwilioForm {
    #[serde(rename = "From")]
    from: String,
    #[serde(rename = "To")]
    to: String,
    #[serde(rename = "Body")]
    body: String,
}

#[derive(Clone)]
struct MockState {
    calls: Arc<Mutex<Vec<Recorded>>>,
    status: StatusCode,
    json_body: String,
    text_body: String,
}

/// Spawns the mock Twilio server and returns `(base_url, recorded_calls)`.
async fn spawn_mock(
    status: StatusCode,
    json_body: &str,
    text_body: &str,
) -> (String, Arc<Mutex<Vec<Recorded>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let state = MockState {
        calls: calls.clone(),
        status,
        json_body: json_body.to_string(),
        text_body: text_body.to_string(),
    };

    let app = Router::new()
        .route("/2010-04-01/Accounts/:sid/Messages.json", post(handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    (format!("http://{addr}"), calls)
}

async fn handler(
    State(state): State<MockState>,
    Path(sid): Path<String>,
    headers: HeaderMap,
    Form(form): Form<TwilioForm>,
) -> (StatusCode, String) {
    state.calls.lock().unwrap().push(Recorded {
        account_sid: sid,
        authorization: headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string),
        form,
    });
    if state.status.is_success() {
        (state.status, state.json_body.clone())
    } else {
        (state.status, state.text_body.clone())
    }
}

// --- pyfly: test_send_builds_request_and_parses_sent_result ----------------

#[tokio::test]
async fn send_builds_request_and_parses_sent_result() {
    let (base, calls) = spawn_mock(StatusCode::CREATED, r#"{"sid":"SM_provider_abc"}"#, "").await;

    let provider = TwilioSmsProvider::new("AC_sid_123", "tok_secret")
        .with_from_number("+15550001111")
        .with_base_url(&base);

    let message = SmsMessage::new("+15559876543", "hello world");
    let id = message.id.clone();
    let result = provider.send(message).await.expect("send");

    // (a) the outbound request the adapter built.
    let recorded = calls.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    let call = &recorded[0];
    assert_eq!(call.account_sid, "AC_sid_123");
    assert_eq!(
        call.form,
        TwilioForm {
            from: "+15550001111".into(),
            to: "+15559876543".into(),
            body: "hello world".into(),
        }
    );
    // HTTP basic auth = base64("AC_sid_123:tok_secret").
    let expected_auth = format!("Basic {}", base64_encode(b"AC_sid_123:tok_secret"));
    assert_eq!(call.authorization.as_deref(), Some(expected_auth.as_str()));

    // (b) the adapter parsed the response into its domain return type.
    assert_eq!(
        result,
        NotificationResult {
            id,
            provider: "twilio".into(),
            status: DeliveryStatus::Sent,
            provider_id: Some("SM_provider_abc".into()),
            error: None,
        }
    );
}

// --- pyfly: test_send_prefers_message_sender_over_provider_from ------------

#[tokio::test]
async fn send_prefers_message_sender_over_provider_from() {
    let (base, calls) = spawn_mock(StatusCode::CREATED, r#"{"sid":"SM_xyz"}"#, "").await;

    let provider = TwilioSmsProvider::new("AC_sid_123", "tok_secret")
        .with_from_number("+15550001111")
        .with_base_url(&base);

    let message = SmsMessage::new("+15559876543", "hi").with_sender("+15552223333");
    provider.send(message).await.expect("send");

    // message.sender wins over the provider's configured from_number.
    let recorded = calls.lock().unwrap();
    assert_eq!(recorded[0].form.from, "+15552223333");
}

// --- pyfly: test_send_maps_non_2xx_to_failed_result ------------------------

#[tokio::test]
async fn send_maps_non_2xx_to_failed_result() {
    let (base, _calls) = spawn_mock(
        StatusCode::UNAUTHORIZED,
        "",
        r#"{"code": 20003, "message": "Authenticate"}"#,
    )
    .await;

    let provider = TwilioSmsProvider::new("AC_sid_123", "tok_secret")
        .with_from_number("+15550001111")
        .with_base_url(&base);

    let result = provider
        .send(SmsMessage::new("+15559876543", "nope"))
        .await
        .expect("send returns Ok with FAILED status, never errors on non-2xx");

    assert_eq!(result.status, DeliveryStatus::Failed);
    assert_eq!(result.provider, "twilio");
    assert_eq!(result.provider_id, None);
    let error = result.error.expect("error populated");
    assert!(error.contains("http 401"), "error was: {error}");
    assert!(error.contains("Authenticate"), "error was: {error}");
}

// --- pyfly: test_send_without_any_sender_raises ----------------------------

#[tokio::test]
async fn send_without_any_sender_raises() {
    // No from_number on the provider, no sender on the message.
    let (base, calls) = spawn_mock(StatusCode::CREATED, r#"{"sid":"SM_unused"}"#, "").await;
    let provider = TwilioSmsProvider::new("AC_sid_123", "tok_secret").with_base_url(&base);

    let err = provider
        .send(SmsMessage::new("+15559876543", "orphan"))
        .await
        .expect_err("missing sender should error before any HTTP call");
    assert_eq!(err, TwilioError::MissingSender);
    assert!(err.to_string().contains("needs a sender"));

    // nothing should have been sent over the wire.
    assert!(calls.lock().unwrap().is_empty());
}

// --- Rust-specific: the SmsProvider port and name --------------------------

#[tokio::test]
async fn provider_satisfies_object_safe_port() {
    let provider: Arc<dyn SmsProvider> =
        Arc::new(TwilioSmsProvider::new("AC", "tok").with_from_number("+1"));
    assert_eq!(provider.name(), "twilio");
}

/// Minimal standard base64 (no external dep needed for the test).
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((n >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(n & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}
