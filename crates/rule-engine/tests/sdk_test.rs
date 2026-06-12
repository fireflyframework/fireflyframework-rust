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

//! SDK client tests — the client's `HttpTransport` port is implemented
//! over the in-process axum router via `tower::ServiceExt::oneshot`,
//! proving the full client ↔ web contract without sockets.

use async_trait::async_trait;
use axum::body::Body;
use axum::Router;
use http::Request;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

use firefly_rule_engine::{
    rule_engine_router, HttpTransport, Logic, Op, Rule, RuleEngineClient, RuleSet, SdkError,
};

/// Drives the rule-engine router in-process: each `post_json` becomes a
/// `oneshot` call against a clone of the router.
struct RouterTransport {
    router: Router,
}

#[async_trait]
impl HttpTransport for RouterTransport {
    async fn post_json(&self, url: &str, body: &Value) -> Result<(u16, Vec<u8>), SdkError> {
        let request = Request::builder()
            .method("POST")
            .uri(url)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(body).unwrap()))
            .unwrap();
        let response = self
            .router
            .clone()
            .oneshot(request)
            .await
            .map_err(|e| SdkError::Transport(e.to_string()))?;
        let status = response.status().as_u16();
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|e| SdkError::Transport(e.to_string()))?
            .to_bytes();
        Ok((status, bytes.to_vec()))
    }
}

fn in_process_client(base_url: &str) -> RuleEngineClient {
    RuleEngineClient::with_transport(
        base_url,
        Arc::new(RouterTransport {
            router: rule_engine_router(),
        }),
    )
}

fn fact(v: Value) -> serde_json::Map<String, Value> {
    v.as_object().unwrap().clone()
}

#[tokio::test]
async fn sdk_evaluate_round_trip() {
    let client = in_process_client("http://rules.local");
    let set = RuleSet::new("orders").with_rule(Rule::new(
        "high-value",
        Logic::cond("amount", Op::Gt, json!(1000.0)),
    ));
    let verdict = client
        .evaluate(&set, &fact(json!({"amount": 1500})))
        .await
        .unwrap();
    assert_eq!(verdict.matched, ["high-value"]);
    assert!(verdict.actions.is_empty());
}

#[tokio::test]
async fn sdk_evaluate_yaml_round_trip() {
    let client = in_process_client("http://rules.local");
    let yaml = "\
name: vip-tagging
rules:
  - id: vip
    when:
      cond: { path: user.referral, op: isNotNull }
    then:
      - type: tag
        params: { name: vip }
";
    let verdict = client
        .evaluate_yaml(yaml, &fact(json!({"user": {"referral": "abc"}})))
        .await
        .unwrap();
    assert_eq!(verdict.matched, ["vip"]);
    assert_eq!(verdict.actions[0].action_type, "tag");
    assert_eq!(verdict.actions[0].params["name"], json!("vip"));
}

#[tokio::test]
async fn sdk_tolerates_trailing_slash_in_base_url() {
    let client = in_process_client("http://rules.local/");
    let set = RuleSet::new("x").with_rule(Rule::new("always", Logic::default()));
    let verdict = client.evaluate(&set, &fact(json!({}))).await.unwrap();
    assert_eq!(verdict.matched, ["always"]);
}

#[tokio::test]
async fn sdk_surfaces_server_errors() {
    let client = in_process_client("http://rules.local");
    let set = RuleSet::new("x").with_rule(Rule::new(
        "r",
        Logic::cond("a", Op::Other("fuzzy".into()), json!(1)),
    ));
    let err = client
        .evaluate(&set, &fact(json!({"a": 1})))
        .await
        .unwrap_err();
    match err {
        SdkError::Http { status, message } => {
            assert_eq!(status, 400);
            assert_eq!(message, "rule \"r\": ruleengine: unknown op: fuzzy");
        }
        other => panic!("expected SdkError::Http, got {other:?}"),
    }
}

#[tokio::test]
async fn sdk_surfaces_yaml_parse_errors() {
    let client = in_process_client("http://rules.local");
    let err = client
        .evaluate_yaml("rules: {not-a-list: true}", &fact(json!({})))
        .await
        .unwrap_err();
    match err {
        SdkError::Http { status, message } => {
            assert_eq!(status, 400);
            assert!(
                message.starts_with("ruleengine: invalid rule DSL:"),
                "message: {message}"
            );
        }
        other => panic!("expected SdkError::Http, got {other:?}"),
    }
}

#[test]
fn sdk_types_are_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<RuleEngineClient>();
    assert_send_sync::<SdkError>();
}
