//! In-process tests for the named-ruleset service router — the router is
//! driven through `tower::ServiceExt::oneshot`, no sockets involved.
//!
//! These port pyfly's `TestEvaluateByName` end-to-end scenarios over the
//! REST surface: register a ruleset, then evaluate it by name and observe
//! the action-mutated facts in the response.

use std::sync::Arc;

use axum::body::Body;
use axum::Router;
use http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use firefly_rule_engine::{rule_engine_service_router_with, RuleEngineService};

async fn send(router: Router, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

fn simple_ruleset() -> Value {
    // r1: when active == true → set result = "matched"
    json!({
        "name": "ignored-the-url-wins",
        "rules": [{
            "id": "r1",
            "when": {"cond": {"path": "active", "op": "eq", "value": true}},
            "then": [{"type": "set", "params": {"target": "result", "value": "matched"}}]
        }]
    })
}

#[tokio::test]
async fn register_then_evaluate_by_name_executes_actions() {
    let service = Arc::new(RuleEngineService::in_memory());
    let router = || rule_engine_service_router_with(service.clone());

    // Register under /test-rs (URL is authoritative for the name).
    let (status, body) = send(
        router(),
        "PUT",
        "/api/rules/rulesets/test-rs",
        simple_ruleset(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"name": "test-rs"}));

    // Evaluate by name with a matching fact.
    let (status, body) = send(
        router(),
        "POST",
        "/api/rules/rulesets/test-rs/evaluate",
        json!({"fact": {"active": true}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["matched"], json!(["r1"]));
    assert_eq!(body["facts"]["result"], json!("matched"));
    assert_eq!(body["facts"]["active"], json!(true));
    assert_eq!(body["error"], Value::Null);
    assert_eq!(
        body["actionsExecuted"],
        json!([{"type": "set", "params": {"target": "result", "value": "matched"}}])
    );
}

#[tokio::test]
async fn evaluate_by_name_no_match_leaves_facts_untouched() {
    let service = Arc::new(RuleEngineService::in_memory());
    let router = || rule_engine_service_router_with(service.clone());

    send(
        router(),
        "PUT",
        "/api/rules/rulesets/test-rs",
        simple_ruleset(),
    )
    .await;

    let (status, body) = send(
        router(),
        "POST",
        "/api/rules/rulesets/test-rs/evaluate",
        json!({"fact": {"active": false}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["matched"], json!([]));
    assert_eq!(body["facts"], json!({"active": false}));
}

#[tokio::test]
async fn evaluate_unknown_ruleset_is_not_found() {
    let service = Arc::new(RuleEngineService::in_memory());
    let (status, body) = send(
        rule_engine_service_router_with(service),
        "POST",
        "/api/rules/rulesets/missing/evaluate",
        json!({"fact": {}}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["error"].as_str().unwrap().contains("missing"));
}

#[tokio::test]
async fn evaluate_missing_fact_defaults_to_empty_object() {
    let service = Arc::new(RuleEngineService::in_memory());
    let router = || rule_engine_service_router_with(service.clone());
    // A ruleset whose only rule has no `when` always fires its set action.
    let rs = json!({
        "name": "x",
        "rules": [{"id": "always", "when": {}, "then": [
            {"type": "set", "params": {"target": "stamped", "value": 1}}
        ]}]
    });
    send(router(), "PUT", "/api/rules/rulesets/x", rs).await;

    let (status, body) = send(
        router(),
        "POST",
        "/api/rules/rulesets/x/evaluate",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["matched"], json!(["always"]));
    assert_eq!(body["facts"], json!({"stamped": 1}));
}

#[tokio::test]
async fn evaluate_unknown_op_is_bad_request() {
    let service = Arc::new(RuleEngineService::in_memory());
    let router = || rule_engine_service_router_with(service.clone());
    let rs = json!({
        "name": "e",
        "rules": [{"id": "r", "when": {"cond": {"path": "a", "op": "fuzzy", "value": 1}}, "then": []}]
    });
    send(router(), "PUT", "/api/rules/rulesets/e", rs).await;

    let (status, body) = send(
        router(),
        "POST",
        "/api/rules/rulesets/e/evaluate",
        json!({"fact": {"a": 1}}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body,
        json!({"error": "rule \"r\": ruleengine: unknown op: fuzzy"})
    );
}

#[tokio::test]
async fn list_rulesets_returns_registered_names_sorted() {
    let service = Arc::new(RuleEngineService::in_memory());
    let router = || rule_engine_service_router_with(service.clone());
    send(router(), "PUT", "/api/rules/rulesets/b", simple_ruleset()).await;
    send(router(), "PUT", "/api/rules/rulesets/a", simple_ruleset()).await;

    let (status, body) = send(router(), "GET", "/api/rules/rulesets", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"names": ["a", "b"]}));
}

#[tokio::test]
async fn unregistered_action_error_is_reported_in_outcome() {
    // A matched action with an unknown type fails in isolation: the
    // response is still 200 with the error recorded.
    let service = Arc::new(RuleEngineService::in_memory());
    let router = || rule_engine_service_router_with(service.clone());
    let rs = json!({
        "name": "err-rs",
        "rules": [{"id": "bad", "when": {}, "then": [
            {"type": "nonexistent_action", "params": {"target": "x"}},
            {"type": "set", "params": {"target": "ok", "value": true}}
        ]}]
    });
    send(router(), "PUT", "/api/rules/rulesets/err-rs", rs).await;

    let (status, body) = send(
        router(),
        "POST",
        "/api/rules/rulesets/err-rs/evaluate",
        json!({"fact": {}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["matched"], json!(["bad"]));
    assert_eq!(body["facts"]["ok"], json!(true), "sibling set still runs");
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("nonexistent_action"));
    assert_eq!(
        body["actionsExecuted"],
        json!([{"type": "set", "params": {"target": "ok", "value": true}}])
    );
}
