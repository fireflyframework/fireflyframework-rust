//! REST admin surface — the Rust counterpart of the Go
//! `ruleengine/web` package (planned for v26.06 in Go; implemented
//! here).
//!
//! The router exposes stateless evaluation endpoints:
//!
//! | Method | Path                        | Body                                  |
//! |--------|-----------------------------|---------------------------------------|
//! | `POST` | `/api/rules/evaluate`       | `{"ruleset": <RuleSet>, "fact": {…}}` |
//! | `POST` | `/api/rules/evaluate/yaml`  | `{"yaml": "<DSL>", "fact": {…}}`      |
//!
//! Both respond `200 OK` with the JSON [`Verdict`]
//! (`{"matched": […], "actions": […]}`) or `400 Bad Request` with
//! `{"error": "<message>"}` when the YAML cannot be parsed or
//! evaluation fails (unknown operator, bad regex, non-numeric
//! comparison).
//!
//! # Quick start
//!
//! ```rust,no_run
//! use firefly_rule_engine::rule_engine_router;
//!
//! #[tokio::main]
//! async fn main() {
//!     let app = rule_engine_router();
//!     let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
//!     axum::serve(listener, app).await.unwrap();
//! }
//! ```

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::core::AstEvaluator;
use crate::interfaces::{Evaluator, Fact, Verdict};
use crate::models::RuleSet;

/// Request body of `POST /api/rules/evaluate`: an inline AST rule set
/// plus the fact to judge it against.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluateRequest {
    /// The rule set, in its canonical JSON projection.
    pub ruleset: RuleSet,
    /// The fact object; defaults to empty.
    #[serde(default)]
    pub fact: Fact,
}

/// Request body of `POST /api/rules/evaluate/yaml`: a YAML rule
/// document plus the fact to judge it against.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluateYamlRequest {
    /// The YAML rule DSL document, verbatim.
    pub yaml: String,
    /// The fact object; defaults to empty.
    #[serde(default)]
    pub fact: Fact,
}

/// JSON error envelope returned with `400 Bad Request`:
/// `{"error": "<message>"}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    /// Human-readable description of what was rejected.
    pub error: String,
}

/// Builds the rule-engine [`Router`] backed by the default
/// [`AstEvaluator`].
pub fn rule_engine_router() -> Router {
    rule_engine_router_with(Arc::new(AstEvaluator::new()))
}

/// Builds the rule-engine [`Router`] backed by a custom
/// [`Evaluator`] implementation.
pub fn rule_engine_router_with(evaluator: Arc<dyn Evaluator>) -> Router {
    Router::new()
        .route("/api/rules/evaluate", post(evaluate))
        .route("/api/rules/evaluate/yaml", post(evaluate_yaml))
        .with_state(evaluator)
}

fn bad_request(message: String) -> Response {
    (StatusCode::BAD_REQUEST, Json(ErrorBody { error: message })).into_response()
}

async fn run(evaluator: &dyn Evaluator, set: &RuleSet, fact: &Fact) -> Response {
    match evaluator.evaluate(set, fact).await {
        Ok(verdict) => (StatusCode::OK, Json::<Verdict>(verdict)).into_response(),
        Err(e) => bad_request(e.to_string()),
    }
}

async fn evaluate(
    State(evaluator): State<Arc<dyn Evaluator>>,
    Json(req): Json<EvaluateRequest>,
) -> Response {
    run(evaluator.as_ref(), &req.ruleset, &req.fact).await
}

async fn evaluate_yaml(
    State(evaluator): State<Arc<dyn Evaluator>>,
    Json(req): Json<EvaluateYamlRequest>,
) -> Response {
    match RuleSet::from_yaml(&req.yaml) {
        Ok(set) => run(evaluator.as_ref(), &set, &req.fact).await,
        Err(e) => bad_request(e.to_string()),
    }
}
