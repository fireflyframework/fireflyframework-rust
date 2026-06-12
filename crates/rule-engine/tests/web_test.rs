//! In-process tests for the REST admin surface — the router is driven
//! through `tower::ServiceExt::oneshot`, no sockets involved.

use axum::body::Body;
use http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use firefly_rule_engine::rule_engine_router;

async fn post_json(uri: &str, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let response = rule_engine_router().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

fn high_value_ruleset() -> Value {
    json!({
        "name": "orders",
        "rules": [{
            "id": "high-value",
            "when": {"cond": {"path": "amount", "op": "gt", "value": 1000.0}},
            "then": [{"type": "review", "params": {"queue": "manual"}}]
        }]
    })
}

#[tokio::test]
async fn evaluate_returns_verdict() {
    let (status, body) = post_json(
        "/api/rules/evaluate",
        json!({"ruleset": high_value_ruleset(), "fact": {"amount": 1500}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({
            "matched": ["high-value"],
            "actions": [{"type": "review", "params": {"queue": "manual"}}]
        })
    );
}

#[tokio::test]
async fn evaluate_with_no_match_returns_empty_verdict() {
    let (status, body) = post_json(
        "/api/rules/evaluate",
        json!({"ruleset": high_value_ruleset(), "fact": {"amount": 10}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"matched": [], "actions": []}));
}

#[tokio::test]
async fn evaluate_missing_fact_defaults_to_empty_object() {
    let (status, body) = post_json(
        "/api/rules/evaluate",
        json!({"ruleset": {"name": "x", "rules": [{"id": "always", "when": {}, "then": []}]}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["matched"], json!(["always"]));
}

#[tokio::test]
async fn evaluate_unknown_op_is_bad_request() {
    let (status, body) = post_json(
        "/api/rules/evaluate",
        json!({
            "ruleset": {"name": "x", "rules": [{
                "id": "r", "when": {"cond": {"path": "a", "op": "fuzzy", "value": 1}}, "then": []
            }]},
            "fact": {"a": 1}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body,
        json!({"error": "rule \"r\": ruleengine: unknown op: fuzzy"})
    );
}

#[tokio::test]
async fn evaluate_yaml_returns_verdict() {
    let yaml = "\
name: vip-tagging
version: 1
rules:
  - id: vip
    when:
      any:
        - cond: { path: user.spend, op: gt, value: 1000 }
        - cond: { path: user.referral, op: isNotNull }
    then:
      - type: tag
        params: { name: vip }
";
    let (status, body) = post_json(
        "/api/rules/evaluate/yaml",
        json!({"yaml": yaml, "fact": {"user": {"spend": 500, "referral": "abc"}}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({"matched": ["vip"], "actions": [{"type": "tag", "params": {"name": "vip"}}]})
    );
}

#[tokio::test]
async fn evaluate_yaml_with_invalid_document_is_bad_request() {
    let (status, body) = post_json(
        "/api/rules/evaluate/yaml",
        json!({"yaml": "rules: {not-a-list: true}", "fact": {}}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let message = body["error"].as_str().unwrap();
    assert!(
        message.starts_with("ruleengine: invalid rule DSL:"),
        "error: {message}"
    );
}

#[tokio::test]
async fn malformed_json_body_is_a_client_error() {
    let request = Request::builder()
        .method("POST")
        .uri("/api/rules/evaluate")
        .header("content-type", "application/json")
        .body(Body::from("{not json"))
        .unwrap();
    let response = rule_engine_router().oneshot(request).await.unwrap();
    assert!(response.status().is_client_error());
}

#[tokio::test]
async fn unknown_route_is_not_found() {
    let request = Request::builder()
        .method("POST")
        .uri("/api/rules/nope")
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let response = rule_engine_router().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
