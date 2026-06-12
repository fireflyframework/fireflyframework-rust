//! Behavior tests for [`FirebasePushProvider`] (FCM HTTP v1), ported 1:1 from
//! pyfly's `tests/notifications/test_firebase_behavior.py`.
//!
//! Where pyfly injects a fake `httpx` client that replays a queue of canned
//! responses and records the call kwargs, this Rust port spins up an in-process
//! axum mock on `127.0.0.1:0` that replays a response queue and records the
//! *actual* request the adapter put on the wire (URL, `Authorization` header,
//! and the JSON `message` payload). No network, no Docker.

use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::Json;
use axum::Router;
use serde_json::{json, Value};

use firefly_notifications_firebase::{
    DeliveryStatus, FirebasePushProvider, PushMessage, PushProvider,
};

/// A canned response the mock replays, in order, one per inbound request.
#[derive(Clone)]
struct CannedResponse {
    status: StatusCode,
    body: String,
}

impl CannedResponse {
    fn ok(json_body: &str) -> Self {
        CannedResponse {
            status: StatusCode::OK,
            body: json_body.to_string(),
        }
    }
    fn err(status: StatusCode, text: &str) -> Self {
        CannedResponse {
            status,
            body: text.to_string(),
        }
    }
}

/// One recorded inbound request.
#[derive(Clone, Debug)]
struct Recorded {
    project_id: String,
    action: String,
    authorization: Option<String>,
    body: Value,
}

#[derive(Clone)]
struct MockState {
    calls: Arc<Mutex<Vec<Recorded>>>,
    responses: Arc<Mutex<std::collections::VecDeque<CannedResponse>>>,
}

/// Spawns the mock FCM server with a queue of responses; returns
/// `(base_url, recorded_calls)`.
async fn spawn_mock(responses: Vec<CannedResponse>) -> (String, Arc<Mutex<Vec<Recorded>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let state = MockState {
        calls: calls.clone(),
        responses: Arc::new(Mutex::new(responses.into_iter().collect())),
    };

    // FCM's path segment `messages:send` contains a literal colon, which axum
    // would misparse as a path param mid-segment; capture the tail wildcard
    // instead and assert on it inside the handler.
    let app = Router::new()
        .route("/v1/projects/:project/*action", post(handler))
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
    Path((project, action)): Path<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, String) {
    state.calls.lock().unwrap().push(Recorded {
        project_id: project,
        action,
        authorization: headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string),
        body,
    });
    let canned = state
        .responses
        .lock()
        .unwrap()
        .pop_front()
        .expect("mock ran out of canned responses");
    (canned.status, canned.body)
}

// --- pyfly: test_send_success_builds_request_and_parses_message_name -------

#[tokio::test]
async fn send_success_builds_request_and_parses_message_name() {
    let (base, calls) = spawn_mock(vec![CannedResponse::ok(
        r#"{"name":"projects/my-proj/messages/0:abc"}"#,
    )])
    .await;

    let provider = FirebasePushProvider::new("my-proj", "ya29.token").with_base_url(&base);

    let mut data = serde_json::Map::new();
    data.insert("badge".into(), json!(3));
    data.insert("deep_link".into(), json!("app://home"));
    let msg = PushMessage::new(["device-token-1"], "Hello", "World").with_data(data);
    let id = msg.id.clone();

    let result = provider.send(msg).await.expect("send");

    // (a) outbound request the adapter built.
    let recorded = calls.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    let call = &recorded[0];
    assert_eq!(call.project_id, "my-proj");
    assert_eq!(call.action, "messages:send");
    assert_eq!(call.authorization.as_deref(), Some("Bearer ya29.token"));
    let payload = &call.body["message"];
    assert_eq!(payload["token"], json!("device-token-1"));
    assert_eq!(
        payload["notification"],
        json!({"title": "Hello", "body": "World"})
    );
    // data values are coerced to strings by the adapter.
    assert_eq!(
        payload["data"],
        json!({"badge": "3", "deep_link": "app://home"})
    );

    // (b) response parsed into the domain result.
    assert_eq!(result.id, id);
    assert_eq!(result.provider, "firebase");
    assert_eq!(result.status, DeliveryStatus::Sent);
    assert_eq!(
        result.provider_id.as_deref(),
        Some("projects/my-proj/messages/0:abc")
    );
    assert_eq!(result.error, None);
}

// --- regression: FCM data-map stringification matches pyfly's str(v) -------
//
// pyfly builds `data` as `{k: str(v) for k, v in message.data.items()}`, so
// every non-string value is rendered by CPython's `str()`/`repr()`: a JSON
// bool `true` -> "True", `false` -> "False", null -> "None", an array -> the
// `", "`-separated Python list repr, and an object -> the Python dict repr with
// single-quoted keys. serde_json's `to_string()` would instead emit "true",
// "null", "[1,2]", and `{"a":1}` — all wire-divergent from the reference. This
// asserts the exact strings the adapter puts on the wire.
#[tokio::test]
async fn send_stringifies_data_values_like_pythons_str() {
    let (base, calls) = spawn_mock(vec![CannedResponse::ok(r#"{"name":"n/1"}"#)]).await;

    let provider = FirebasePushProvider::new("my-proj", "ya29.token").with_base_url(&base);

    let mut data = serde_json::Map::new();
    // scalars that already matched (kept to guard against regressions).
    data.insert("int".into(), json!(42));
    data.insert("float".into(), json!(1.5));
    data.insert("str".into(), json!("plain"));
    // the divergent cases.
    data.insert("bool_t".into(), json!(true));
    data.insert("bool_f".into(), json!(false));
    data.insert("nil".into(), Value::Null);
    data.insert("arr".into(), json!([1, 2]));
    data.insert("mixed_arr".into(), json!([1, "x", true, null]));
    data.insert("obj".into(), json!({ "a": 1 }));
    data.insert(
        "nested".into(),
        json!({ "a": [1, true], "b": { "c": null } }),
    );
    // a string whose Python repr would switch to double quotes (lives inside an
    // array, so it is repr'd, not passed through).
    data.insert("apostrophe".into(), json!(["it's"]));

    let msg = PushMessage::new(["device-1"], "t", "b").with_data(data);
    provider.send(msg).await.expect("send");

    let recorded = calls.lock().unwrap();
    let sent = &recorded[0].body["message"]["data"];
    assert_eq!(
        *sent,
        json!({
            "int": "42",
            "float": "1.5",
            "str": "plain",
            "bool_t": "True",
            "bool_f": "False",
            "nil": "None",
            "arr": "[1, 2]",
            "mixed_arr": "[1, 'x', True, None]",
            "obj": "{'a': 1}",
            "nested": "{'a': [1, True], 'b': {'c': None}}",
            "apostrophe": "[\"it's\"]",
        }),
    );
}

// --- pyfly: test_send_error_response_maps_to_failed_result -----------------

#[tokio::test]
async fn send_error_response_maps_to_failed_result() {
    let (base, _calls) = spawn_mock(vec![CannedResponse::err(
        StatusCode::NOT_FOUND,
        "registration token not found",
    )])
    .await;

    let provider = FirebasePushProvider::new("my-proj", "ya29.token").with_base_url(&base);

    let result = provider
        .send(PushMessage::new(["stale-token"], "t", "b"))
        .await
        .expect("send returns Ok with FAILED status");

    assert_eq!(result.status, DeliveryStatus::Failed);
    assert_eq!(result.provider_id, None);
    assert_eq!(result.error.as_deref(), Some("stale-token: http 404"));
}

// --- pyfly: test_send_multi_token_partial_success_is_sent_with_error -------

#[tokio::test]
async fn send_multi_token_partial_success_is_sent_with_error() {
    let (base, calls) = spawn_mock(vec![
        CannedResponse::ok(r#"{"name":"projects/my-proj/messages/ok-1"}"#),
        CannedResponse::err(StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
    ])
    .await;

    let provider = FirebasePushProvider::new("my-proj", "ya29.token").with_base_url(&base);

    let msg = PushMessage::new(["good", "bad"], "t", "b");
    let result = provider.send(msg).await.expect("send");

    // one request per device token, in order.
    let recorded = calls.lock().unwrap();
    let tokens: Vec<String> = recorded
        .iter()
        .map(|c| c.body["message"]["token"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(tokens, vec!["good".to_string(), "bad".to_string()]);

    // partial success: at least one delivered => SENT, but failures recorded.
    assert_eq!(result.status, DeliveryStatus::Sent);
    assert_eq!(
        result.provider_id.as_deref(),
        Some("projects/my-proj/messages/ok-1")
    );
    assert_eq!(result.error.as_deref(), Some("bad: http 503"));
}

// --- Rust-specific: refreshing token source + object-safe port -------------

#[tokio::test]
async fn token_provider_is_invoked_per_send_and_can_refresh() {
    let (base, calls) = spawn_mock(vec![CannedResponse::ok(r#"{"name":"n/1"}"#)]).await;

    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let c2 = counter.clone();
    let provider: Arc<dyn PushProvider> = Arc::new(
        FirebasePushProvider::with_token_provider("my-proj", move || {
            let n = c2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(format!("ya29.token-{n}"))
        })
        .with_base_url(&base),
    );
    assert_eq!(provider.name(), "firebase");

    provider
        .send(PushMessage::new(["t"], "hi", "there"))
        .await
        .expect("send");

    let recorded = calls.lock().unwrap();
    assert_eq!(
        recorded[0].authorization.as_deref(),
        Some("Bearer ya29.token-0")
    );
}
